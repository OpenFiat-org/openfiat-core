//! The replicated index of merchant-defined payment methods.
//!
//! This is what makes the feature more than the bug it replaces. The
//! picker in one interface used to let a merchant "add" a method and then
//! wrote it to `localStorage` — under a footnote claiming it had been
//! "shared in the registry". It had not: the merchant's counterparty, on
//! another node and in another browser, saw an advertisement naming
//! something nothing could resolve.
//!
//! A definition here is a signed record that travels by gossip like every
//! other record in this workspace, is stored by every node, and is
//! readable by anyone. Whether the *author* is who they say they are is
//! settled by the signature; whether the *name* is one anybody should be
//! shown is settled by [`crate::record::MerchantPaymentMethod::validate`]
//! before it is stored.

use crate::error::TaxonomyError;
use crate::events::SignedPaymentMethodDefine;
use crate::protocol;
use crate::record::{MerchantPaymentMethod, PaymentMethodRef};
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, PeerId};

/// This crate's column family.
///
/// Public because a node's composition root has to list every column
/// family it opens *before* any of them is written to — see
/// `openfiat_rpc::state::SNAPSHOT_COLUMN_FAMILIES`. A registry whose
/// family is missing from that list writes into nothing on a real RocksDB
/// node while passing every in-memory test.
pub const COLUMN_FAMILY: &str = "payment_methods";

/// How many definitions one merchant's peer id may hold.
///
/// These records are gossiped to every node and kept forever, and unlike
/// an advertisement a definition costs its author nothing to make — no
/// liquidity, no counterparty, no trade. Unbounded, one wallet is a
/// permanent write amplifier against every node's disk. Thirty-two is far
/// past what a real merchant needs (the whole of this build's catalog is
/// a hundred and fifty-odd rails across every country on earth) and small
/// enough that a flood is pointless.
pub const MAX_METHODS_PER_MERCHANT: usize = 32;

pub struct PaymentMethodRegistry<S> {
    store: S,
}

impl<S: KvStore> PaymentMethodRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get(&self, id: &PaymentMethodRef) -> Option<MerchantPaymentMethod> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    /// Every definition this merchant has published, by id, ascending.
    ///
    /// Ascending by id and not by anything else, because the id is the
    /// only total order every node agrees on — see [`Self::apply_define`]
    /// for why that ordering is also what decides which definitions
    /// survive the bound.
    pub fn for_merchant(
        &self,
        merchant: &PeerId,
    ) -> Vec<(PaymentMethodRef, MerchantPaymentMethod)> {
        let mut found: Vec<(PaymentMethodRef, MerchantPaymentMethod)> = self
            .store
            .iter_prefix(COLUMN_FAMILY, prefix_of(merchant).as_bytes())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(key, value)| {
                let id = PaymentMethodRef::parse(std::str::from_utf8(&key).ok()?).ok()?;
                Some((id, wire::from_bytes(&value).ok()?))
            })
            .collect();
        found.sort_by(|(a, _), (b, _)| a.cmp(b));
        found
    }

    pub fn all(&self) -> Vec<MerchantPaymentMethod> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// Stores a signed definition after every check that needs no state.
    ///
    /// # Republishing is a no-op, not a conflict
    ///
    /// The id is the digest of the record, so a second copy of a
    /// definition is byte-identical to the first and there is nothing to
    /// resolve. That is the reason this store needs none of the
    /// arrival-order tie-breaking every other registry here does: two
    /// nodes cannot hold different records under one id.
    ///
    /// # Which definitions survive the bound, and why it is not "the first
    /// thirty-two"
    ///
    /// The thirty-two with the lexicographically smallest ids. First-heard
    /// would be a rule whose answer depends on the order gossip happened
    /// to deliver in, so two nodes would keep different subsets of the same
    /// merchant's methods — and an advertisement would resolve on one node
    /// and not on its neighbour. Ordering by id is a property of the
    /// records themselves, so every node converges on the same thirty-two
    /// however it heard them.
    ///
    /// Nothing outside this merchant's own prefix can be displaced by it,
    /// so the bound is self-inflicted and cannot be used against anybody
    /// else.
    ///
    /// # Errors
    ///
    /// [`TaxonomyError::InvalidSignature`] for a record that is not the
    /// merchant's, [`TaxonomyError::MalformedDefinition`] or
    /// [`TaxonomyError::ImpersonatesKnownMethod`] for a name that cannot
    /// be shown, and [`TaxonomyError::TooManyMethods`] for one that does
    /// not fit under the bound.
    pub fn apply_define(
        &self,
        signed: SignedPaymentMethodDefine,
    ) -> Result<PaymentMethodRef, TaxonomyError> {
        signed.verify()?;
        signed.method.validate()?;
        let id = signed.method.id();
        if self.get(&id).is_some() {
            return Ok(id);
        }

        let held = self.for_merchant(&signed.method.merchant);
        if held.len() >= MAX_METHODS_PER_MERCHANT {
            let (largest, _) = held.last().expect("a full shelf is not an empty one");
            if &id >= largest {
                return Err(TaxonomyError::TooManyMethods);
            }
            let _ = self
                .store
                .delete(COLUMN_FAMILY, largest.as_str().as_bytes());
        }

        if let Ok(bytes) = wire::to_bytes(&signed.method) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, id.as_str().as_bytes(), &bytes);
        }
        Ok(id)
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC
            || event.event_type.as_str() != protocol::EVENT_DEFINED
        {
            return;
        }
        if let Ok(signed) = wire::from_bytes(&event.payload) {
            let _ = self.apply_define(signed);
        }
    }
}

/// The key prefix every one of a merchant's definitions shares.
///
/// The separator is part of it deliberately: without the colon, one peer
/// id that is a prefix of another would sweep up the other's records, and
/// base58 has no length delimiter to stop it.
fn prefix_of(merchant: &PeerId) -> String {
    format!("{merchant}:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{keypair, merchant_method, peer};
    use crate::record::PaymentMethodCategory;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::ErrorCode;

    fn define(
        registry: &PaymentMethodRegistry<MemoryStore>,
        seed: u8,
        name: &str,
    ) -> Result<PaymentMethodRef, TaxonomyError> {
        registry.apply_define(SignedPaymentMethodDefine::sign(
            merchant_method(seed, name),
            &keypair(seed),
        ))
    }

    #[test]
    fn a_merchants_definition_is_stored_and_reads_back_as_they_wrote_it() {
        let registry = PaymentMethodRegistry::new(MemoryStore::new());
        let id = define(&registry, 1, "Acme Pay").expect("a well-signed definition is accepted");

        let stored = registry.get(&id).expect("it must be readable by anyone");
        assert_eq!(stored.name, "Acme Pay");
        assert_eq!(stored.merchant, peer(1));
        assert_eq!(registry.for_merchant(&peer(1)).len(), 1);
        assert!(
            registry.for_merchant(&peer(2)).is_empty(),
            "a definition belongs to its author and to no one else"
        );
    }

    /// The rug-pull, attempted. The merchant republishes under a new name
    /// hoping to change what an advertisement already points at.
    #[test]
    fn a_second_definition_never_overwrites_the_first() {
        let registry = PaymentMethodRegistry::new(MemoryStore::new());
        let original = define(&registry, 1, "Acme Pay").unwrap();
        let renamed = define(&registry, 1, "PayPal Business").unwrap();

        assert_ne!(original, renamed);
        assert_eq!(
            registry.get(&original).unwrap().name,
            "Acme Pay",
            "an advertisement pointing at the original must still resolve to it"
        );
        assert_eq!(registry.for_merchant(&peer(1)).len(), 2);
    }

    #[test]
    fn republishing_the_same_definition_is_a_no_op_rather_than_a_conflict() {
        let registry = PaymentMethodRegistry::new(MemoryStore::new());
        let first = define(&registry, 1, "Acme Pay").unwrap();
        let again = define(&registry, 1, "Acme Pay").unwrap();
        assert_eq!(first, again);
        assert_eq!(registry.for_merchant(&peer(1)).len(), 1);
    }

    #[test]
    fn a_forged_definition_never_reaches_the_store() {
        let registry = PaymentMethodRegistry::new(MemoryStore::new());
        let forged = SignedPaymentMethodDefine::sign(merchant_method(1, "Acme Pay"), &keypair(2));
        assert_eq!(
            registry.apply_define(forged),
            Err(TaxonomyError::InvalidSignature)
        );
        assert!(registry.all().is_empty());
    }

    /// The bound is only worth having if it is applied before storage.
    #[test]
    fn an_impersonating_name_never_reaches_the_store() {
        let registry = PaymentMethodRegistry::new(MemoryStore::new());
        assert_eq!(
            define(&registry, 1, "МPesa"),
            Err(TaxonomyError::ImpersonatesKnownMethod)
        );
        assert_eq!(
            define(&registry, 1, "Acme\u{202E}Pay"),
            Err(TaxonomyError::MalformedDefinition)
        );
        assert!(registry.all().is_empty());
    }

    /// A full shelf is a count, and the code has to say so.
    ///
    /// This used to answer `RATE_LIMIT_EXCEEDED`, which is the one code a
    /// generic client is most likely to handle automatically — and it
    /// handles it by waiting and sending the same request again. Nothing
    /// about waiting frees a slot here: the cap does not decay, and the
    /// only thing that opens one is the merchant retiring a definition or
    /// publishing one that sorts ahead of what they already hold. A
    /// client told to back off backs off forever.
    ///
    /// Asserted on the OFS-8000 code rather than the variant because the
    /// variant was never wrong. What crossed the wire was.
    #[test]
    fn a_merchant_past_the_cap_is_told_the_shelf_is_full_not_that_they_are_going_too_fast() {
        let registry = PaymentMethodRegistry::new(MemoryStore::new());
        // Names chosen so the last one sorts after a full shelf and so
        // displaces nothing — the only case that is actually refused.
        for n in 0..MAX_METHODS_PER_MERCHANT {
            define(&registry, 1, &format!("Acme Rail {n:02}")).expect("under the cap");
        }
        let mut refused = Err(TaxonomyError::InvalidSignature);
        for suffix in 'a'..='z' {
            refused = define(&registry, 1, &format!("Acme Rail zz{suffix}"));
            if refused.is_err() {
                break;
            }
        }

        assert_eq!(refused, Err(TaxonomyError::TooManyMethods));
        let code = TaxonomyError::TooManyMethods.code();
        assert_eq!(code, ErrorCode::PaymentMethodLimitReached);
        assert!(
            !code.retryable(),
            "{} is flagged retryable, so a client will send this same definition again \
             forever. The cap is a count, not a speed.",
            code.name()
        );
    }

    /// Two nodes hear one merchant's flood in opposite orders. They must
    /// end up holding the same set, or an advertisement resolves on one
    /// and not on the other.
    #[test]
    fn two_nodes_keep_the_same_methods_whatever_order_they_hear_them_in() {
        let flood: Vec<_> = (0..MAX_METHODS_PER_MERCHANT + 8)
            .map(|n| {
                SignedPaymentMethodDefine::sign(
                    merchant_method(1, &format!("Acme Rail {n}")),
                    &keypair(1),
                )
            })
            .collect();

        let mut kept = Vec::new();
        for reversed in [false, true] {
            let registry = PaymentMethodRegistry::new(MemoryStore::new());
            let mut heard = flood.clone();
            if reversed {
                heard.reverse();
            }
            for signed in heard {
                let _ = registry.apply_define(signed);
            }
            let ids: Vec<String> = registry
                .for_merchant(&peer(1))
                .into_iter()
                .map(|(id, _)| id.as_str().to_string())
                .collect();
            assert_eq!(ids.len(), MAX_METHODS_PER_MERCHANT);
            kept.push(ids);
        }
        assert_eq!(
            kept[0], kept[1],
            "arrival order must not decide what survives"
        );
    }

    /// One merchant filling their own shelf must not touch anybody
    /// else's.
    #[test]
    fn a_flood_is_confined_to_the_wallet_that_published_it() {
        let registry = PaymentMethodRegistry::new(MemoryStore::new());
        define(&registry, 2, "Neighbour Pay").unwrap();
        for n in 0..MAX_METHODS_PER_MERCHANT + 4 {
            let _ = define(&registry, 1, &format!("Acme Rail {n}"));
        }
        assert_eq!(
            registry.for_merchant(&peer(1)).len(),
            MAX_METHODS_PER_MERCHANT
        );
        assert_eq!(registry.for_merchant(&peer(2)).len(), 1);
    }

    #[test]
    fn a_gossiped_event_from_another_spec_is_ignored() {
        let registry = PaymentMethodRegistry::new(MemoryStore::new());
        let payload = wire::to_bytes(&SignedPaymentMethodDefine::sign(
            merchant_method(1, "Acme Pay"),
            &keypair(1),
        ))
        .unwrap();
        let mut envelope = EventEnvelope {
            id: openfiat_types::EventId::from_bytes([7; 32]),
            event_type: openfiat_types::EventType::new(protocol::EVENT_DEFINED).unwrap(),
            ofs_spec: 9999,
            version: 1,
            origin: peer(1),
            timestamp: openfiat_types::Timestamp::from_millis(1),
            ttl: 8,
            priority: openfiat_types::Priority::Reputation,
            signature: openfiat_types::Signature::from_bytes([0u8; 64]),
            payload,
        };
        registry.apply_event(&envelope);
        assert!(registry.all().is_empty());

        envelope.ofs_spec = protocol::OFS_SPEC;
        registry.apply_event(&envelope);
        assert_eq!(registry.all().len(), 1);
    }

    /// A definition's category travels with it: a picker that grouped a
    /// merchant's own rail under the wrong heading would be showing a
    /// bank transfer among the cash options.
    #[test]
    fn a_definition_keeps_the_category_its_author_chose() {
        let registry = PaymentMethodRegistry::new(MemoryStore::new());
        let mut method = merchant_method(1, "Acme Till");
        method.category = PaymentMethodCategory::Cash;
        let id = registry
            .apply_define(SignedPaymentMethodDefine::sign(method, &keypair(1)))
            .unwrap();
        assert_eq!(
            registry.get(&id).unwrap().published().category,
            PaymentMethodCategory::Cash
        );
    }
}
