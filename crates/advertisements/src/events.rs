//! Signed advertisement lifecycle events (§23): Created, a status change,
//! a terms change, and a merchant-initiated price update (§17's "Price
//! changes" refresh trigger). Liquidity changes are deliberately *not* a
//! signed event type here — §10 says inventory management is automatic,
//! driven by reservation/settlement activity, not a fresh merchant
//! signature per trade (see
//! [`crate::store::AdvertisementRegistry::reserve_liquidity`]).
//!
//! # What a merchant could not do until these existed
//!
//! Two things, and both of them are ordinary daily operations rather than
//! edge cases.
//!
//! An advertisement's status was one-way. The only status event was
//! `Disable`, so of the four states [`crate::record::AdvertisementStatus`]
//! defines, three were unreachable: a merchant could not go on vacation
//! (§16), could not delete an ad (§21), and — worst — could not turn an ad
//! back on. An ad auto-disabled by §18 when its liquidity hit zero stayed
//! disabled forever, however much liquidity the merchant added afterwards.
//! [`AdvertisementStatusSet`] replaces `Disable` rather than joining it:
//! two events that both set a status is a rule with two places to be
//! wrong.
//!
//! Trade limits and payment methods were fixed at creation. A merchant who
//! wanted to raise their ceiling or add a payment method had to disable
//! the advertisement and publish a new one, losing its id — and with it
//! every reservation, settlement and review that referenced it.
//! [`AdvertisementTermsUpdate`] changes them in place.

use crate::error::AdvertisementError;
use crate::record::{AdvertisementId, AdvertisementStatus, Direction, PricingModel};
use openfiat_crypto::{Keypair, MintAddress, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_taxonomy::PaymentMethodRef;
use openfiat_types::{Amount, FiatCurrency, PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdvertisementCreate {
    pub id: AdvertisementId,
    pub merchant: PeerId,
    pub merchant_public_key: PublicKey,
    pub asset_mint: MintAddress,
    pub direction: Direction,
    /// The fiat side of the pair, as an ISO 4217 code.
    ///
    /// Was a bare `String` that nothing validated, so `KES`, `kes`,
    /// `Kenyan Shillings` and `""` were all equally acceptable on a
    /// signed, replicated record — which meant an order book could show
    /// one corridor under several headings and a filter had to compare
    /// case-insensitively to work at all. `FiatCurrency` normalises at
    /// the door, so equality means what it looks like it means.
    ///
    /// Checked for *form*, never for membership of a list. See
    /// `openfiat_types::currency` — and `PricingModel::Floating`'s
    /// `price_decimals` above, which reached the same conclusion first.
    pub fiat_currency: FiatCurrency,
    pub min_trade: Amount,
    pub max_trade: Amount,
    pub initial_liquidity: Amount,
    pub pricing: PricingModel,
    /// By method id, never by name — see
    /// [`crate::record::Advertisement::payment_methods`].
    pub payment_methods: Vec<PaymentMethodRef>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedAdvertisementCreate {
    pub create: AdvertisementCreate,
    pub signature: Signature,
}

impl SignedAdvertisementCreate {
    pub fn sign(create: AdvertisementCreate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::ADVERTISEMENT_CREATE,
            &create,
        )
        .expect("AdvertisementCreate always serializes");
        Self {
            signature: keypair.sign(&bytes),
            create,
        }
    }

    /// Verify the signature and that the claimed merchant Peer ID derives
    /// from the claimed public key (peer-poisoning defense, same pattern
    /// used throughout this workspace).
    pub fn verify(&self) -> Result<(), AdvertisementError> {
        let expected = peer_id_from_public_key(&self.create.merchant_public_key)
            .map_err(|_| AdvertisementError::InvalidSignature)?;
        if expected != self.create.merchant {
            return Err(AdvertisementError::UnauthorizedUpdate);
        }
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::ADVERTISEMENT_CREATE,
            &self.create,
        )
        .map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        verify(&self.create.merchant_public_key, &bytes, &self.signature)
            .map_err(|_| AdvertisementError::InvalidSignature)
    }
}

/// A merchant moving their advertisement between the states in
/// [`AdvertisementStatus`] — pausing it for a holiday, taking it down,
/// deleting it, or putting it back up.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdvertisementStatusSet {
    pub id: AdvertisementId,
    pub merchant: PeerId,
    pub status: AdvertisementStatus,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedAdvertisementStatusSet {
    pub set: AdvertisementStatusSet,
    pub signature: Signature,
}

impl SignedAdvertisementStatusSet {
    pub fn sign(set: AdvertisementStatusSet, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::ADVERTISEMENT_STATUS_SET,
            &set,
        )
        .expect("AdvertisementStatusSet always serializes");
        Self {
            signature: keypair.sign(&bytes),
            set,
        }
    }
}

/// A merchant changing what they will trade, without losing the
/// advertisement's identity.
///
/// Every field is the new value in full, not a delta. A partial update
/// would mean "unchanged" and "cleared" look identical on the wire for
/// `payment_methods`, and a merchant removing their last payment method
/// is a thing that must be refusable rather than ambiguous.
///
/// The price is *not* here. It changes far more often than these do —
/// [`AdvertisementPriceUpdate`] exists for exactly that reason — and
/// folding the two together would make every price tick restate the trade
/// limits, so a stale client that re-sent an old form would silently roll
/// them back.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdvertisementTermsUpdate {
    pub id: AdvertisementId,
    pub merchant: PeerId,
    /// Denominated in the asset, like the record's own fields.
    pub min_trade: Amount,
    pub max_trade: Amount,
    /// By method id, never by name — see
    /// [`crate::record::Advertisement::payment_methods`].
    pub payment_methods: Vec<PaymentMethodRef>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedAdvertisementTermsUpdate {
    pub update: AdvertisementTermsUpdate,
    pub signature: Signature,
}

impl SignedAdvertisementTermsUpdate {
    pub fn sign(update: AdvertisementTermsUpdate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::ADVERTISEMENT_TERMS_UPDATE,
            &update,
        )
        .expect("AdvertisementTermsUpdate always serializes");
        Self {
            signature: keypair.sign(&bytes),
            update,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdvertisementPriceUpdate {
    pub id: AdvertisementId,
    pub merchant: PeerId,
    pub pricing: PricingModel,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedAdvertisementPriceUpdate {
    pub update: AdvertisementPriceUpdate,
    pub signature: Signature,
}

impl SignedAdvertisementPriceUpdate {
    pub fn sign(update: AdvertisementPriceUpdate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::ADVERTISEMENT_PRICE_UPDATE,
            &update,
        )
        .expect("AdvertisementPriceUpdate always serializes");
        Self {
            signature: keypair.sign(&bytes),
            update,
        }
    }
}
