//! Signed advertisement lifecycle events (§23): Created, Disabled, and a
//! merchant-initiated price update (§17's "Price changes" refresh
//! trigger). Liquidity changes are deliberately *not* a signed event type
//! here — §10 says inventory management is automatic, driven by
//! reservation/settlement activity, not a fresh merchant signature per
//! trade (see [`crate::store::AdvertisementRegistry::reserve_liquidity`]).

use crate::error::AdvertisementError;
use crate::record::{AdvertisementId, Direction, PricingModel};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{Amount, PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdvertisementCreate {
    pub id: AdvertisementId,
    pub merchant: PeerId,
    pub merchant_public_key: PublicKey,
    pub asset: String,
    pub direction: Direction,
    pub fiat_currency: String,
    pub min_trade: Amount,
    pub max_trade: Amount,
    pub initial_liquidity: Amount,
    pub pricing: PricingModel,
    pub payment_methods: Vec<String>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedAdvertisementCreate {
    pub create: AdvertisementCreate,
    pub signature: Signature,
}

impl SignedAdvertisementCreate {
    pub fn sign(create: AdvertisementCreate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&create).expect("AdvertisementCreate always serializes");
        Self { signature: keypair.sign(&bytes), create }
    }

    /// Verify the signature and that the claimed merchant Peer ID derives
    /// from the claimed public key (peer-poisoning defense, same pattern
    /// used throughout this workspace).
    pub fn verify(&self) -> Result<(), AdvertisementError> {
        let expected = peer_id_from_public_key(&self.create.merchant_public_key).map_err(|_| AdvertisementError::InvalidSignature)?;
        if expected != self.create.merchant {
            return Err(AdvertisementError::UnauthorizedUpdate);
        }
        let bytes = openfiat_serialization::wire::to_bytes(&self.create).map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        verify(&self.create.merchant_public_key, &bytes, &self.signature).map_err(|_| AdvertisementError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdvertisementDisable {
    pub id: AdvertisementId,
    pub merchant: PeerId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedAdvertisementDisable {
    pub disable: AdvertisementDisable,
    pub signature: Signature,
}

impl SignedAdvertisementDisable {
    pub fn sign(disable: AdvertisementDisable, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&disable).expect("AdvertisementDisable always serializes");
        Self { signature: keypair.sign(&bytes), disable }
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
        let bytes = openfiat_serialization::wire::to_bytes(&update).expect("AdvertisementPriceUpdate always serializes");
        Self { signature: keypair.sign(&bytes), update }
    }
}
