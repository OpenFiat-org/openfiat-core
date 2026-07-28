//! Signed reservation events (§6, §14): a request (self-consistency
//! verified, like every other signed creation event in this workspace)
//! and a cancellation (verified against the reservation's on-file owner,
//! like a registry/advertisement update).
//!
//! Expiration is deliberately *not* a signed event — see
//! [`crate::store::ReservationRegistry::expire_stale`].

use crate::error::ReservationError;
use crate::record::ReservationId;
use openfiat_advertisements::AdvertisementId;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{Amount, PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReservationRequest {
    pub id: ReservationId,
    pub advertisement_id: AdvertisementId,
    pub requester: PeerId,
    pub requester_public_key: PublicKey,
    pub amount: Amount,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedReservationRequest {
    pub request: ReservationRequest,
    pub signature: Signature,
}

impl SignedReservationRequest {
    pub fn sign(request: ReservationRequest, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&request).expect("ReservationRequest always serializes");
        Self { signature: keypair.sign(&bytes), request }
    }

    pub fn verify(&self) -> Result<(), ReservationError> {
        let expected = peer_id_from_public_key(&self.request.requester_public_key).map_err(|_| ReservationError::InvalidSignature)?;
        if expected != self.request.requester {
            return Err(ReservationError::UnauthorizedUpdate);
        }
        let bytes = openfiat_serialization::wire::to_bytes(&self.request).map_err(|_| ReservationError::MalformedReservation)?;
        verify(&self.request.requester_public_key, &bytes, &self.signature).map_err(|_| ReservationError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReservationCancel {
    pub id: ReservationId,
    pub requester: PeerId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedReservationCancel {
    pub cancel: ReservationCancel,
    pub signature: Signature,
}

impl SignedReservationCancel {
    pub fn sign(cancel: ReservationCancel, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&cancel).expect("ReservationCancel always serializes");
        Self { signature: keypair.sign(&bytes), cancel }
    }
}
