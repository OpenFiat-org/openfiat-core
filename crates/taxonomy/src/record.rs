//! What a payment method is, how an advertisement names one, and what a
//! merchant is allowed to invent.

use crate::error::TaxonomyError;
use crate::name::{check_name, skeleton};
use openfiat_crypto::sha256;
use openfiat_types::{PeerId, PublicKey};

/// The namespace of every method compiled into a build.
///
/// Not a valid peer id and never will be: `PeerId`'s `Display` is
/// base58btc, whose alphabet omits `l` precisely because it is easy to
/// confuse with `1`. So a merchant cannot register under a peer id that
/// spells `builtin`, and the two halves of the namespace cannot be
/// confused for one another.
pub const BUILTIN: &str = "builtin";

/// Which kind of rail a payment method is, so an interface can group a
/// long list into something a person can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaymentMethodCategory {
    MobileMoney,
    BankTransfer,
    Fintech,
    /// OFS-2100 §13 names Cash Deposit alongside the electronic rails.
    /// Cash is the only method that exists in every country, which is the
    /// point of listing it: a market with no local electronic system is
    /// still tradeable.
    Cash,
}

/// How an advertisement names a payment method.
///
/// # Why an id and not a name
///
/// An advertisement used to carry the method's display name as free text.
/// That is a rug-pull: whoever controls the name controls what every
/// advertisement that chose it appears to offer, after the fact and
/// without touching the advertisement. Rename your own "Acme Pay" to
/// "PayPal" and every ad that picked it now claims to take PayPal.
///
/// So an advertisement carries this, and the name is resolved at the edge
/// — the same conclusion `Advertisement::asset_mint` reached when it
/// stopped being the string `"USDC"`, and `openfiat_registry`'s
/// `ServicePricing::token_mint` before that. A method whose name this
/// build cannot resolve is displayed as its id, which is ugly and honest.
///
/// Both halves of the namespace are immutable by construction:
///
/// - `builtin:<slug>` is a row compiled into the node. The slug is a
///   column of [`crate::catalog`], never derived from the display name,
///   so correcting a spelling cannot orphan an advertisement.
/// - `<peer id>:<digest>` is a merchant's own definition, and the digest
///   is of the definition itself (see [`MerchantPaymentMethod::id`]).
///   Editing anything about it produces a different id, which existing
///   advertisements do not reference. There is no edit path because there
///   is nothing an edit could land on.
///
/// # Form is checked; membership never is
///
/// `builtin:the-rail-added-next-year` parses on a node built today and is
/// stored on an advertisement exactly as one this build knows. Checking
/// membership of the catalog would mean a node one release behind
/// rejecting a perfectly good advertisement, and two honest nodes
/// disagreeing about which advertisements exist —
/// `openfiat_types::FiatCurrency` sets out that argument in full and this
/// is the same argument about the same kind of table.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct PaymentMethodRef(String);

impl PaymentMethodRef {
    /// Parse a reference in either namespace.
    ///
    /// # Errors
    ///
    /// [`TaxonomyError::MalformedDefinition`] for anything that is not
    /// exactly one `namespace:body` pair in one of the two shapes above.
    /// Strict on purpose: this is what a signed, replicated record
    /// carries, so an unparseable value must fail at the door rather than
    /// be stored and re-interpreted by every later reader.
    pub fn parse(input: &str) -> Result<Self, TaxonomyError> {
        let (namespace, body) = input
            .split_once(':')
            .ok_or(TaxonomyError::MalformedDefinition)?;
        if body.contains(':') {
            return Err(TaxonomyError::MalformedDefinition);
        }
        let well_formed = if namespace == BUILTIN {
            is_slug(body)
        } else {
            is_peer_id(namespace) && is_digest(body)
        };
        if !well_formed {
            return Err(TaxonomyError::MalformedDefinition);
        }
        Ok(Self(input.to_string()))
    }

    /// A reference to a row of this build's own catalog.
    ///
    /// # Errors
    ///
    /// [`TaxonomyError::MalformedDefinition`] if `slug` is not lowercase
    /// alphanumerics separated by single hyphens.
    pub fn builtin(slug: &str) -> Result<Self, TaxonomyError> {
        Self::parse(&format!("{BUILTIN}:{slug}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The peer id this reference belongs to, or `None` for a built-in.
    ///
    /// Returned as the spelling rather than a `PeerId` because that is
    /// what the comparison in [`Self::is_selectable_by`] needs, and
    /// because a reference is not required to name a peer this node has
    /// ever heard of.
    pub fn owner(&self) -> Option<&str> {
        let (namespace, _) = self.0.split_once(':')?;
        (namespace != BUILTIN).then_some(namespace)
    }

    /// Whether `merchant` may put this reference on their own
    /// advertisement.
    ///
    /// This is where the scoping decision is enforced, and it is worth
    /// being explicit about what it costs and buys. A merchant-defined
    /// method is **selectable only by the merchant who defined it**, and
    /// **readable by everybody** — it replicates by gossip like every
    /// other record here, so a counterparty on another node resolves the
    /// name and sees who wrote it.
    ///
    /// The alternative — a definition anyone may select — makes one global
    /// namespace for arbitrary merchant text. The first merchant to define
    /// "Chase Bank Transfer" would own how that rail appears to everyone
    /// forever, and every other merchant's use of it would be an
    /// endorsement of a stranger's record. Scoping it removes the prize
    /// for squatting entirely: a name is only ever worth what the merchant
    /// who wrote it is worth, and two merchants who both take Acme Pay
    /// publish one definition each.
    ///
    /// The cost is duplication — the same rail defined many times under
    /// many ids — and that is the right trade, because those duplicates
    /// are exactly what a global namespace hides.
    ///
    /// Note what this does *not* need: any lookup. The owner is inside the
    /// reference, so an advertisement naming a definition that has not
    /// replicated to this node yet is still checkable, and gossip arrival
    /// order cannot make the same advertisement valid on one node and
    /// invalid on another.
    pub fn is_selectable_by(&self, merchant: &PeerId) -> bool {
        match self.owner() {
            None => true,
            Some(owner) => owner == merchant.to_string(),
        }
    }
}

impl std::fmt::Display for PaymentMethodRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for PaymentMethodRef {
    type Error = TaxonomyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<PaymentMethodRef> for String {
    fn from(value: PaymentMethodRef) -> Self {
        value.0
    }
}

fn is_slug(body: &str) -> bool {
    !body.is_empty()
        && body.len() <= 48
        && !body.starts_with('-')
        && !body.ends_with('-')
        && !body.contains("--")
        && body
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The base58btc alphabet, which omits `0`, `O`, `I` and `l`.
fn is_peer_id(body: &str) -> bool {
    (16..=64).contains(&body.len())
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() && !matches!(c, '0' | 'O' | 'I' | 'l'))
}

fn is_digest(body: &str) -> bool {
    body.len() == DIGEST_HEX_LEN
        && body
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
}

/// Characters of SHA-256 that identify a merchant's definition.
///
/// Sixteen hex characters is 64 bits, over a namespace scoped to one
/// merchant's own [`crate::store::MAX_METHODS_PER_MERCHANT`] entries. A
/// collision would mean a merchant colliding with themselves, and there is
/// nothing to gain by it — both preimages would be their own definitions.
const DIGEST_HEX_LEN: usize = 16;

/// A payment method a merchant can advertise accepting.
///
/// Serves both halves of the taxonomy: a row of [`crate::catalog`] and a
/// merchant's own definition are handed to a client in this same shape, so
/// a picker renders them with one code path. `id` is what tells them apart
/// — see [`PaymentMethodRef`] — and a client is required to say which is
/// which; see `docs/payment-methods.md`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PaymentMethod {
    /// What an advertisement stores. The only field anything keys off.
    pub id: PaymentMethodRef,
    /// The name as it should be shown. Never compared, never stored on an
    /// advertisement.
    pub name: String,
    pub category: PaymentMethodCategory,
    /// Spellings a person might type when they mean this method, for
    /// type-ahead. Lowercase. Never shown. Always empty for a
    /// merchant-defined method: an alias is a claim about what a name
    /// means, and letting a merchant claim that "mpesa" means their
    /// method would hand back exactly the impersonation that
    /// [`MerchantPaymentMethod::validate`] refuses.
    pub aliases: Vec<String>,
    /// Countries this rail is suggested in, most relevant first. `None`
    /// means this build makes no per-country claim — see
    /// [`crate::catalog`] for why that is an answer rather than a gap.
    /// Always `None` for a merchant-defined method, which is offered to
    /// its own author wherever they are and to nobody else.
    pub countries: Option<Vec<String>>,
}

/// A rail a merchant settles on that this build has never heard of.
///
/// # What is deliberately not here
///
/// An id. It is derived — see [`Self::id`] — so a merchant cannot choose
/// one, cannot land on another merchant's, and cannot publish two
/// different definitions under one identifier.
///
/// A timestamp. The record is immutable and idempotent: republishing it is
/// a no-op because the second copy has the same id and the same bytes.
/// Carrying a timestamp would make the same definition published twice
/// into two definitions, and would give a later record something to differ
/// by — which is the whole mechanism a rug-pull needs.
///
/// Countries and aliases. Both are claims a merchant has no standing to
/// make: a definition is selectable only by its author, so "which
/// countries is it suggested in" has one answer (theirs), and an alias
/// would let them claim a well-known name through the back door.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MerchantPaymentMethod {
    pub merchant: PeerId,
    pub merchant_public_key: PublicKey,
    /// The merchant's own words, bounded and checked — see
    /// [`crate::name::check_name`] for what a client is guaranteed about
    /// this string, and [`Self::validate`] for what it may not resemble.
    pub name: String,
    pub category: PaymentMethodCategory,
}

impl MerchantPaymentMethod {
    /// This definition's identifier: the author, then a digest of the
    /// whole record.
    ///
    /// Derived rather than chosen, for the reason `openfiat_reviews::
    /// ReviewId` gives — an author must not be able to squat an identifier
    /// or publish under somebody else's — and content-addressed on top of
    /// that, which buys two more things.
    ///
    /// Convergence is free. Two nodes cannot hold different records under
    /// one id, because the id *is* the record's digest, so the
    /// arrival-order tie-break every other store in this workspace needs
    /// has nothing to decide here.
    ///
    /// And an edit is impossible rather than merely refused. A merchant
    /// who republishes with one word changed produces a different id;
    /// advertisements that named the old one still name the old one, whose
    /// bytes cannot have changed. That is the answer to "a name that can
    /// be edited after ads reference it is a rug-pull vector": there is no
    /// such name.
    ///
    /// # Panics
    ///
    /// Never in practice — the record is plain data and always serializes.
    pub fn id(&self) -> PaymentMethodRef {
        let bytes = openfiat_serialization::json::to_bytes(self)
            .expect("a MerchantPaymentMethod is plain data and always serializes");
        let digest: String = sha256(&bytes)
            .iter()
            .take(DIGEST_HEX_LEN / 2)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        PaymentMethodRef(format!("{}:{digest}", self.merchant))
    }

    /// The same record in the shape a picker renders.
    pub fn published(&self) -> PaymentMethod {
        PaymentMethod {
            id: self.id(),
            name: self.name.clone(),
            category: self.category,
            aliases: Vec::new(),
            countries: None,
        }
    }

    /// Everything checkable from the record alone: the name is one a
    /// client can render, and it is not a look-alike of a rail this build
    /// already ships.
    ///
    /// # The impersonation rule, and its limits
    ///
    /// A definition is refused when its name folds to the same skeleton as
    /// any catalog name *or alias* — so `M-Pesa`, `m pesa`, `МPesa` and
    /// `M-Pes4` are all refused, and so is `Cash`, because that is how
    /// somebody reaches Cash in Person by typing. See
    /// [`crate::name::skeleton`] for exactly what folding covers.
    ///
    /// It is deliberately not checked against *other merchants'*
    /// definitions. Such a check would need the store, and the store's
    /// contents depend on what this node has heard: two nodes would accept
    /// and refuse different records, and the network would disagree about
    /// which definitions exist — the failure `openfiat_reviews::store`
    /// describes at length for authorization checks done at write time.
    /// The scoping rule is what makes that acceptable: another merchant's
    /// look-alike is not selectable by anyone but themselves, and is shown
    /// as theirs.
    ///
    /// # Errors
    ///
    /// [`TaxonomyError::MalformedDefinition`] for an unrenderable name,
    /// [`TaxonomyError::ImpersonatesKnownMethod`] for a look-alike.
    pub fn validate(&self) -> Result<(), TaxonomyError> {
        check_name(&self.name)?;
        let folded = skeleton(&self.name);
        let collides = crate::catalog::catalog().iter().any(|known| {
            skeleton(&known.name) == folded
                || known.aliases.iter().any(|alias| skeleton(alias) == folded)
        });
        if collides {
            return Err(TaxonomyError::ImpersonatesKnownMethod);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{merchant_method, peer};

    #[test]
    fn a_builtin_reference_parses_and_belongs_to_nobody() {
        let reference = PaymentMethodRef::builtin("mpesa-kenya").unwrap();
        assert_eq!(reference.as_str(), "builtin:mpesa-kenya");
        assert_eq!(reference.owner(), None);
        assert!(
            reference.is_selectable_by(&peer(1)) && reference.is_selectable_by(&peer(2)),
            "a compiled-in rail is everybody's to choose"
        );
    }

    /// The scoping decision, as a test rather than a paragraph.
    #[test]
    fn a_merchants_definition_is_selectable_by_that_merchant_and_nobody_else() {
        let mine = merchant_method(1, "Acme Pay").id();
        assert_eq!(mine.owner(), Some(peer(1).to_string()).as_deref());
        assert!(mine.is_selectable_by(&peer(1)));
        assert!(
            !mine.is_selectable_by(&peer(2)),
            "a stranger putting this on their own advertisement would be \
             trading on somebody else's definition"
        );
    }

    #[test]
    fn a_reference_that_is_not_one_is_refused_at_the_door() {
        for hostile in [
            "",
            "mpesa",
            "builtin:",
            "builtin:M-Pesa",
            "builtin:has spaces",
            "builtin:trailing-",
            "builtin:double--hyphen",
            "builtin:a:b",
            "12D3KooWnotlongenough:xyz",
            // A peer-shaped namespace with a body that is not a digest.
            "12D3KooWAAAAAAAAAAAAAAAAAAAAAAAAAAAA:notahexdigest123",
            // Uppercase hex would give one definition two spellings.
            "12D3KooWAAAAAAAAAAAAAAAAAAAAAAAAAAAA:ABCDEF0123456789",
        ] {
            assert!(
                PaymentMethodRef::parse(hostile).is_err(),
                "{hostile:?} must not become a reference"
            );
        }
    }

    /// The id is the whole anti-rug-pull argument, so this is it stated
    /// as behaviour: change any part of the definition and the id moves,
    /// which means every advertisement that chose the old one still has
    /// the old one.
    #[test]
    fn editing_a_definition_produces_a_different_id_rather_than_changing_one() {
        let original = merchant_method(1, "Acme Pay");
        let renamed = merchant_method(1, "PayPal Business");
        assert_ne!(original.id(), renamed.id());

        let mut recategorised = original.clone();
        recategorised.category = PaymentMethodCategory::Cash;
        assert_ne!(original.id(), recategorised.id());

        assert_eq!(
            original.id(),
            merchant_method(1, "Acme Pay").id(),
            "the same definition must be the same id on every node"
        );
    }

    #[test]
    fn two_merchants_defining_the_same_name_get_different_ids() {
        assert_ne!(
            merchant_method(1, "Acme Pay").id(),
            merchant_method(2, "Acme Pay").id()
        );
    }

    /// The attack this feature would otherwise open: a picker entry that
    /// reads as Safaricom's rail.
    #[test]
    fn a_look_alike_of_a_rail_this_build_ships_is_refused() {
        for impostor in [
            "M-Pesa",
            "m pesa",
            "MPESA",
            "М-Реѕа",
            "M-Pes4",
            "Ｍ－Ｐｅｓａ",
            "PIX",
            "p1x",
            // An alias counts: this is how a person reaches Cash in
            // Person by typing, so it is a name that already means
            // something.
            "Cash",
            "Wire",
        ] {
            assert_eq!(
                merchant_method(1, impostor).validate(),
                Err(TaxonomyError::ImpersonatesKnownMethod),
                "{impostor:?} must not become a merchant's own rail"
            );
        }
    }

    #[test]
    fn a_rail_this_build_has_never_heard_of_is_accepted() {
        for genuine in [
            "Acme Pay",
            "Sacco Standing Order",
            "M-Pesa Kenya via Acme Till 4421",
            "支付宝转账",
        ] {
            assert_eq!(
                merchant_method(1, genuine).validate(),
                Ok(()),
                "{genuine:?} is a rail somebody may really settle on"
            );
        }
    }

    #[test]
    fn an_unrenderable_name_never_becomes_a_definition() {
        for hostile in ["Acme\u{202E}Pay", "Acme Pay ", "Acme\u{200B} Pay", ""] {
            assert_eq!(
                merchant_method(1, hostile).validate(),
                Err(TaxonomyError::MalformedDefinition),
                "{hostile:?}"
            );
        }
    }

    /// A definition crosses the wire as the plain string a JSON client
    /// would write, and comes back checked — an unparseable one fails to
    /// deserialize rather than being stored and re-interpreted later.
    #[test]
    fn a_reference_travels_as_a_bare_string_and_is_parsed_on_the_way_back() {
        let reference = PaymentMethodRef::builtin("sepa").unwrap();
        let json = serde_json::to_string(&reference).unwrap();
        assert_eq!(json, "\"builtin:sepa\"");
        assert_eq!(
            serde_json::from_str::<PaymentMethodRef>(&json).unwrap(),
            reference
        );
        assert!(serde_json::from_str::<PaymentMethodRef>("\"M-Pesa\"").is_err());
    }
}
