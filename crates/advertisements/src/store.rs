//! The replicated local advertisement index. Same shape as
//! `openfiat-registry`'s store: generic over `KvStore`, populated purely
//! by consuming gossip events (§23).

use crate::error::AdvertisementError;
use crate::events::{
    SignedAdvertisementCreate, SignedAdvertisementPriceUpdate, SignedAdvertisementStatusSet,
    SignedAdvertisementTermsUpdate,
};
use crate::protocol;
use crate::record::{Advertisement, AdvertisementId, AdvertisementStatus, Direction};
use openfiat_crypto::verify;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_taxonomy::PaymentMethodRef;
use openfiat_types::{Amount, EventEnvelope, PeerId, Timestamp};

const COLUMN_FAMILY: &str = "advertisements";

pub struct AdvertisementRegistry<S> {
    store: S,
}

impl<S: KvStore> AdvertisementRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get(&self, id: &AdvertisementId) -> Option<Advertisement> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, ad: &Advertisement) {
        if let Ok(bytes) = wire::to_bytes(ad) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, ad.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Advertisement> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// Only advertisements currently visible for trading (§8, §16, §18 —
    /// active, not vacationing, not disabled or deleted).
    pub fn find_active(&self, direction: Direction) -> Vec<Advertisement> {
        self.all()
            .into_iter()
            .filter(|ad| ad.direction == direction && ad.status == AdvertisementStatus::Active)
            .collect()
    }

    pub fn apply_create(
        &self,
        signed: SignedAdvertisementCreate,
    ) -> Result<AdvertisementId, AdvertisementError> {
        signed.verify()?;
        let id = signed.create.id.clone();
        if self.get(&id).is_some() {
            return Err(AdvertisementError::DuplicateAdvertisementId);
        }
        // Checked here as well as on a terms update, because an
        // advertisement created unusable is the same problem as one edited
        // into being unusable, and nothing else on the path was checking.
        Self::check_terms(
            &signed.create.min_trade,
            &signed.create.max_trade,
            &signed.create.payment_methods,
            &signed.create.merchant,
        )?;
        let create = signed.create;
        self.put(&Advertisement {
            id: create.id,
            merchant: create.merchant,
            merchant_public_key: create.merchant_public_key,
            asset_mint: create.asset_mint,
            direction: create.direction,
            fiat_currency: create.fiat_currency,
            min_trade: create.min_trade,
            max_trade: create.max_trade,
            available_liquidity: create.initial_liquidity,
            pricing: create.pricing,
            payment_methods: create.payment_methods,
            status: AdvertisementStatus::Active,
            created_at: create.timestamp,
            updated_at: create.timestamp,
        });
        Ok(id)
    }

    /// §16/§18/§21: the merchant moving their own advertisement between
    /// states — pausing it, taking it down, deleting it, or putting it
    /// back up.
    ///
    /// Reactivation is the case this exists for. Until it did, the only
    /// status event was a disable, so an advertisement §18 auto-disabled
    /// when its liquidity hit zero stayed disabled however much liquidity
    /// the merchant added afterwards. Two rules bound it:
    ///
    /// - a deleted advertisement stays deleted (§21). Reviving a retired
    ///   id would make deletion a suggestion, and every reservation and
    ///   settlement that referenced it would suddenly point at a live ad
    ///   again;
    /// - an advertisement with no liquidity cannot be made active,
    ///   because §18 would disable it on the next reservation anyway. The
    ///   alternative is an order book entry that exists to fail.
    pub fn apply_status_set(
        &self,
        signed: SignedAdvertisementStatusSet,
    ) -> Result<(), AdvertisementError> {
        let bytes =
            json::to_bytes(&signed.set).map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        let mut ad = self.authorize(
            &signed.set.id,
            &signed.set.merchant,
            &bytes,
            &signed.signature,
        )?;

        if signed.set.status == AdvertisementStatus::Active
            && ad.available_liquidity.base_units() == 0
        {
            return Err(AdvertisementError::InsufficientLiquidity);
        }

        ad.status = signed.set.status;
        ad.updated_at = signed.set.timestamp;
        self.put(&ad);
        Ok(())
    }

    /// §6: the merchant changing what they will trade, in place.
    ///
    /// The whole value is that the id survives. Republishing under a new
    /// id — which is what a merchant had to do before this — orphans
    /// every reservation, settlement and review that named the old one.
    pub fn apply_terms_update(
        &self,
        signed: SignedAdvertisementTermsUpdate,
    ) -> Result<(), AdvertisementError> {
        let bytes = json::to_bytes(&signed.update)
            .map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        let mut ad = self.authorize(
            &signed.update.id,
            &signed.update.merchant,
            &bytes,
            &signed.signature,
        )?;

        Self::check_terms(
            &signed.update.min_trade,
            &signed.update.max_trade,
            &signed.update.payment_methods,
            &signed.update.merchant,
        )?;

        ad.min_trade = signed.update.min_trade;
        ad.max_trade = signed.update.max_trade;
        ad.payment_methods = signed.update.payment_methods;
        ad.updated_at = signed.update.timestamp;
        self.put(&ad);
        Ok(())
    }

    /// The checks every merchant-signed edit shares: the advertisement
    /// exists, it is not deleted, the signer is its merchant, and the
    /// signature is over what arrived.
    ///
    /// One function rather than three copies, because the copies had
    /// already begun to drift — `apply_disable` and `apply_pricing_update`
    /// both matched the merchant before verifying the signature, which is
    /// harmless but means an unauthenticated caller can distinguish "not
    /// your ad" from "no such ad". Checking authorship *after* the
    /// signature closes that, and closes it in one place.
    fn authorize(
        &self,
        id: &AdvertisementId,
        merchant: &openfiat_types::PeerId,
        signed_bytes: &[u8],
        signature: &openfiat_types::Signature,
    ) -> Result<Advertisement, AdvertisementError> {
        let ad = self
            .get(id)
            .ok_or(AdvertisementError::AdvertisementNotFound)?;
        verify(&ad.merchant_public_key, signed_bytes, signature)
            .map_err(|_| AdvertisementError::InvalidSignature)?;
        if ad.merchant != *merchant {
            return Err(AdvertisementError::UnauthorizedUpdate);
        }
        if ad.status == AdvertisementStatus::Deleted {
            return Err(AdvertisementError::AdvertisementDeleted);
        }
        Ok(ad)
    }

    pub fn apply_pricing_update(
        &self,
        signed: SignedAdvertisementPriceUpdate,
    ) -> Result<(), AdvertisementError> {
        let bytes = json::to_bytes(&signed.update)
            .map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        let mut ad = self.authorize(
            &signed.update.id,
            &signed.update.merchant,
            &bytes,
            &signed.signature,
        )?;

        ad.pricing = signed.update.pricing;
        ad.updated_at = signed.update.timestamp;
        self.put(&ad);
        Ok(())
    }

    /// §9-10: lock `amount` of an advertisement's available liquidity —
    /// the effect a reservation has on the ad it was opened against. Not
    /// a signed merchant action; driven by protocol events from whichever
    /// crate manages reservations. Automatically disables the ad if this
    /// exhausts its liquidity (§18).
    pub fn reserve_liquidity(
        &self,
        id: &AdvertisementId,
        amount: Amount,
    ) -> Result<(), AdvertisementError> {
        let mut ad = self
            .get(id)
            .ok_or(AdvertisementError::AdvertisementNotFound)?;
        let remaining = ad
            .available_liquidity
            .checked_sub(amount)
            .ok_or(AdvertisementError::InsufficientLiquidity)?;
        ad.available_liquidity = remaining;
        if remaining.base_units() == 0 {
            ad.status = AdvertisementStatus::Disabled;
        }
        ad.updated_at = Timestamp::now();
        self.put(&ad);
        Ok(())
    }

    /// The inverse of [`Self::reserve_liquidity`] — a cancelled or
    /// expired reservation returns its amount to the pool (§10).
    pub fn release_liquidity(
        &self,
        id: &AdvertisementId,
        amount: Amount,
    ) -> Result<(), AdvertisementError> {
        let mut ad = self
            .get(id)
            .ok_or(AdvertisementError::AdvertisementNotFound)?;
        ad.available_liquidity = ad
            .available_liquidity
            .checked_add(amount)
            .ok_or(AdvertisementError::MalformedAdvertisement)?;
        ad.updated_at = Timestamp::now();
        self.put(&ad);
        Ok(())
    }

    /// Apply a gossip event to this index, if it's one of ours.
    /// Terms an advertisement cannot be traded against.
    ///
    /// A floor above the ceiling matches nothing, and an advertisement
    /// with no payment method has no way for a buyer to pay. Both are
    /// worse than an advertisement that does not exist, because both show
    /// up in the order book and fail at reservation — after a buyer has
    /// chosen them.
    ///
    /// Decimals must agree, for the reason [`Amount::checked_add`] gives:
    /// comparing a floor in lamports against a ceiling in cents produces a
    /// number, and it is not an answer to the question anyone asked.
    ///
    /// # And a method has to be one this merchant may offer
    ///
    /// A `PaymentMethodRef` has already been checked for form by the time
    /// it arrives — that happens where it is deserialized, so a malformed
    /// one never becomes a value. What is checked here is the one thing
    /// that needs the record around it: a merchant may name this build's
    /// own rails, and their own definitions, and not another merchant's.
    ///
    /// Putting a stranger's definition on your advertisement would be
    /// trading on their record — their name, their category, and their
    /// ability to be the only account that can be paid on it — and there
    /// is no reason to do it that publishing your own definition does not
    /// serve. `openfiat_taxonomy::PaymentMethodRef::is_selectable_by`
    /// carries the whole argument for scoping definitions this way.
    ///
    /// It needs no lookup and no state: the owner is inside the reference.
    /// So this cannot depend on whether the definition has reached this
    /// node yet, and the same advertisement is valid on every node
    /// regardless of gossip arrival order.
    fn check_terms(
        min_trade: &Amount,
        max_trade: &Amount,
        payment_methods: &[PaymentMethodRef],
        merchant: &PeerId,
    ) -> Result<(), AdvertisementError> {
        if payment_methods.is_empty() {
            return Err(AdvertisementError::UnusableTerms);
        }
        if !payment_methods
            .iter()
            .all(|method| method.is_selectable_by(merchant))
        {
            return Err(AdvertisementError::UnusableTerms);
        }
        if min_trade.decimals() != max_trade.decimals()
            || min_trade.base_units() > max_trade.base_units()
        {
            return Err(AdvertisementError::UnusableTerms);
        }
        Ok(())
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match event.event_type.as_str() {
            protocol::EVENT_CREATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_create(signed);
                }
            }
            protocol::EVENT_STATUS_SET => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_status_set(signed);
                }
            }
            protocol::EVENT_TERMS_UPDATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_terms_update(signed);
                }
            }
            protocol::EVENT_PRICING_UPDATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_pricing_update(signed);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AdvertisementCreate;
    use crate::record::PricingModel;
    use openfiat_crypto::Keypair;
    use openfiat_crypto::MintAddress;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_taxonomy::PaymentMethodRef;
    use openfiat_types::FiatCurrency;

    /// A built-in method reference, since that is what nearly every
    /// fixture here wants: this crate checks the *form* of a reference and
    /// who may select it, never whether the catalog has heard of it.
    fn method(slug: &str) -> PaymentMethodRef {
        PaymentMethodRef::builtin(slug).expect("a fixture slug is well formed")
    }

    fn create(keypair: &Keypair, id: &str, liquidity: u64) -> AdvertisementCreate {
        AdvertisementCreate {
            id: AdvertisementId::new(id),
            merchant: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            merchant_public_key: keypair.public_key(),
            asset_mint: MintAddress::parse("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU").unwrap(),
            direction: Direction::Sell,
            fiat_currency: FiatCurrency::parse("KES").unwrap(),
            min_trade: Amount::new(10_000_000, 6),
            max_trade: Amount::new(10_000_000_000, 6),
            initial_liquidity: Amount::new(liquidity, 6),
            pricing: PricingModel::Fixed {
                price: Amount::new(129_000_000, 6),
            },
            payment_methods: vec![method("mpesa-kenya")],
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn a_created_advertisement_is_active_and_queryable() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&keypair, "ad-1", 10_000_000_000),
                &keypair,
            ))
            .unwrap();
        let ad = registry.get(&id).unwrap();
        assert_eq!(ad.status, AdvertisementStatus::Active);
        assert_eq!(registry.find_active(Direction::Sell), vec![ad]);
    }

    #[test]
    fn reserving_more_than_available_liquidity_is_rejected() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&keypair, "ad-1", 1_000_000),
                &keypair,
            ))
            .unwrap();

        let result = registry.reserve_liquidity(&id, Amount::new(2_000_000, 6));
        assert_eq!(result, Err(AdvertisementError::InsufficientLiquidity));
        assert_eq!(
            registry.get(&id).unwrap().available_liquidity,
            Amount::new(1_000_000, 6)
        );
    }

    #[test]
    fn exhausting_liquidity_automatically_disables_the_advertisement() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&keypair, "ad-1", 1_000_000),
                &keypair,
            ))
            .unwrap();

        registry
            .reserve_liquidity(&id, Amount::new(1_000_000, 6))
            .unwrap();
        let ad = registry.get(&id).unwrap();
        assert_eq!(ad.available_liquidity, Amount::new(0, 6));
        assert_eq!(ad.status, AdvertisementStatus::Disabled);
        assert!(registry.find_active(Direction::Sell).is_empty());
    }

    #[test]
    fn releasing_liquidity_restores_it() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&keypair, "ad-1", 5_000_000),
                &keypair,
            ))
            .unwrap();

        registry
            .reserve_liquidity(&id, Amount::new(2_000_000, 6))
            .unwrap();
        registry
            .release_liquidity(&id, Amount::new(2_000_000, 6))
            .unwrap();
        assert_eq!(
            registry.get(&id).unwrap().available_liquidity,
            Amount::new(5_000_000, 6)
        );
    }

    #[test]
    fn a_status_change_from_a_different_merchant_is_rejected() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&owner, "ad-1", 1_000_000),
                &owner,
            ))
            .unwrap();

        let attacker = Keypair::generate();
        let signed = crate::events::SignedAdvertisementStatusSet::sign(
            crate::events::AdvertisementStatusSet {
                id,
                merchant: peer_id_from_public_key(&owner.public_key()).unwrap(),
                status: AdvertisementStatus::Disabled,
                timestamp: Timestamp::now(),
            },
            &attacker,
        );
        assert_eq!(
            registry.apply_status_set(signed),
            Err(AdvertisementError::InvalidSignature)
        );
    }

    fn status_set(
        keypair: &Keypair,
        id: &AdvertisementId,
        status: AdvertisementStatus,
    ) -> crate::events::SignedAdvertisementStatusSet {
        crate::events::SignedAdvertisementStatusSet::sign(
            crate::events::AdvertisementStatusSet {
                id: id.clone(),
                merchant: peer_id_from_public_key(&keypair.public_key()).unwrap(),
                status,
                timestamp: Timestamp::now(),
            },
            keypair,
        )
    }

    fn live_ad(
        registry: &AdvertisementRegistry<MemoryStore>,
        keypair: &Keypair,
    ) -> AdvertisementId {
        registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(keypair, "ad-1", 1_000_000),
                keypair,
            ))
            .unwrap()
    }

    /// The case the whole event exists for. An advertisement §18
    /// auto-disabled when its liquidity ran out used to stay disabled
    /// forever, however much liquidity the merchant added afterwards —
    /// the only status event was a disable.
    #[test]
    fn a_merchant_can_put_a_disabled_advertisement_back_up() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = live_ad(&registry, &owner);

        registry
            .apply_status_set(status_set(&owner, &id, AdvertisementStatus::Disabled))
            .unwrap();
        registry
            .apply_status_set(status_set(&owner, &id, AdvertisementStatus::Active))
            .unwrap();

        assert_eq!(
            registry.get(&id).unwrap().status,
            AdvertisementStatus::Active
        );
    }

    #[test]
    fn a_merchant_can_go_on_vacation_and_come_back() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = live_ad(&registry, &owner);

        registry
            .apply_status_set(status_set(&owner, &id, AdvertisementStatus::Vacation))
            .unwrap();
        // §16: paused is not the same as broken, and a paused ad is not
        // offered for trading.
        assert!(registry.find_active(Direction::Sell).is_empty());

        registry
            .apply_status_set(status_set(&owner, &id, AdvertisementStatus::Active))
            .unwrap();
        assert_eq!(registry.find_active(Direction::Sell).len(), 1);
    }

    /// §21. Reviving a retired id would make deletion a suggestion, and
    /// every reservation and settlement that named it would point at a
    /// live advertisement again.
    #[test]
    fn a_deleted_advertisement_cannot_be_brought_back_or_edited() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = live_ad(&registry, &owner);

        registry
            .apply_status_set(status_set(&owner, &id, AdvertisementStatus::Deleted))
            .unwrap();

        assert_eq!(
            registry.apply_status_set(status_set(&owner, &id, AdvertisementStatus::Active)),
            Err(AdvertisementError::AdvertisementDeleted)
        );
        assert_eq!(
            registry.apply_terms_update(terms(&owner, &id, 1, 2, &["bank-transfer"])),
            Err(AdvertisementError::AdvertisementDeleted)
        );
    }

    /// §18 again, from the other side: an advertisement with nothing to
    /// sell must not be reactivated into the order book, because the next
    /// reservation would disable it anyway.
    #[test]
    fn an_advertisement_with_no_liquidity_cannot_be_reactivated() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&owner, "ad-1", 0),
                &owner,
            ))
            .unwrap();

        assert_eq!(
            registry.apply_status_set(status_set(&owner, &id, AdvertisementStatus::Active)),
            Err(AdvertisementError::InsufficientLiquidity)
        );
    }

    fn terms(
        keypair: &Keypair,
        id: &AdvertisementId,
        min: u64,
        max: u64,
        methods: &[&str],
    ) -> crate::events::SignedAdvertisementTermsUpdate {
        crate::events::SignedAdvertisementTermsUpdate::sign(
            crate::events::AdvertisementTermsUpdate {
                id: id.clone(),
                merchant: peer_id_from_public_key(&keypair.public_key()).unwrap(),
                min_trade: Amount::new(min, 6),
                max_trade: Amount::new(max, 6),
                payment_methods: methods.iter().map(|m| method(m)).collect(),
                timestamp: Timestamp::now(),
            },
            keypair,
        )
    }

    /// The id surviving is the entire point: republishing under a new one
    /// orphans every reservation, settlement and review that named it.
    #[test]
    fn a_merchant_can_change_limits_and_payment_methods_in_place() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = live_ad(&registry, &owner);

        registry
            .apply_terms_update(terms(
                &owner,
                &id,
                5_000_000,
                50_000_000_000,
                &["bank-transfer", "mpesa-kenya"],
            ))
            .unwrap();

        let ad = registry.get(&id).unwrap();
        assert_eq!(ad.min_trade, Amount::new(5_000_000, 6));
        assert_eq!(ad.max_trade, Amount::new(50_000_000_000, 6));
        assert_eq!(
            ad.payment_methods,
            vec![method("bank-transfer"), method("mpesa-kenya")]
        );
        assert_eq!(ad.id, id);
    }

    #[test]
    fn a_terms_update_from_a_different_merchant_is_rejected() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = live_ad(&registry, &owner);

        let attacker = Keypair::generate();
        let forged = crate::events::SignedAdvertisementTermsUpdate::sign(
            crate::events::AdvertisementTermsUpdate {
                id: id.clone(),
                merchant: peer_id_from_public_key(&owner.public_key()).unwrap(),
                min_trade: Amount::new(1, 6),
                max_trade: Amount::new(2, 6),
                payment_methods: vec![method("bank-transfer")],
                timestamp: Timestamp::now(),
            },
            &attacker,
        );
        assert_eq!(
            registry.apply_terms_update(forged),
            Err(AdvertisementError::InvalidSignature)
        );
        assert_eq!(
            registry.get(&id).unwrap().min_trade,
            Amount::new(10_000_000, 6)
        );
    }

    /// Terms nobody can trade against are worse than no advertisement:
    /// they show up in the order book and fail at reservation, after a
    /// buyer has already chosen them.
    #[test]
    fn terms_that_match_nothing_are_refused_on_update_and_on_create() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = live_ad(&registry, &owner);

        // A floor above the ceiling.
        assert_eq!(
            registry.apply_terms_update(terms(&owner, &id, 100, 10, &["mpesa-kenya"])),
            Err(AdvertisementError::UnusableTerms)
        );
        // No way for a buyer to pay.
        assert_eq!(
            registry.apply_terms_update(terms(&owner, &id, 10, 100, &[])),
            Err(AdvertisementError::UnusableTerms)
        );

        // And the same rule at creation, which nothing was checking.
        let mut unusable = create(&owner, "ad-2", 1_000_000);
        unusable.min_trade = Amount::new(100, 6);
        unusable.max_trade = Amount::new(10, 6);
        assert_eq!(
            registry.apply_create(SignedAdvertisementCreate::sign(unusable, &owner)),
            Err(AdvertisementError::UnusableTerms)
        );
    }

    /// The scoping rule, enforced where it has to be. A merchant may put
    /// their own definition on their own advertisement and may not put a
    /// stranger's — trading on somebody else's record, whose name and
    /// payment account they do not control.
    #[test]
    fn a_merchant_may_offer_their_own_definitions_and_not_another_merchants() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let stranger = Keypair::generate();
        let define = |keypair: &Keypair| {
            openfiat_taxonomy::MerchantPaymentMethod {
                merchant: peer_id_from_public_key(&keypair.public_key()).unwrap(),
                merchant_public_key: keypair.public_key(),
                name: "Acme Pay".to_string(),
                category: openfiat_taxonomy::PaymentMethodCategory::BankTransfer,
            }
            .id()
        };

        let mut mine = create(&owner, "ad-1", 1_000_000);
        mine.payment_methods = vec![define(&owner)];
        assert!(
            registry
                .apply_create(SignedAdvertisementCreate::sign(mine, &owner))
                .is_ok()
        );

        let mut theirs = create(&owner, "ad-2", 1_000_000);
        theirs.payment_methods = vec![define(&stranger)];
        assert_eq!(
            registry.apply_create(SignedAdvertisementCreate::sign(theirs, &owner)),
            Err(AdvertisementError::UnusableTerms),
            "a definition belongs to the wallet that signed it"
        );
    }

    /// Nothing here consults the catalog. A node one release behind must
    /// carry an advertisement naming a rail added since, or two honest
    /// nodes disagree about which advertisements exist.
    #[test]
    fn a_builtin_this_build_has_never_heard_of_is_still_carried() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let mut future = create(&owner, "ad-1", 1_000_000);
        future.payment_methods = vec![method("some-rail-added-next-year")];
        assert!(
            registry
                .apply_create(SignedAdvertisementCreate::sign(future, &owner))
                .is_ok()
        );
    }
}
