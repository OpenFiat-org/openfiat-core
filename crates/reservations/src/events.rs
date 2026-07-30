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
    /// Fiat per unit of asset, as the requester understood it when they
    /// signed.
    ///
    /// A floating advertisement publishes a *formula*, not a price, and
    /// two nodes resolving it at the same instant can legitimately return
    /// different numbers. Before this field existed, a taker agreed to a
    /// number the protocol recorded nowhere: a merchant could later assert
    /// a different rate and there was nothing to hold them to, because
    /// nothing had ever been written down.
    ///
    /// The reservation is where the agreement happens, so this is where
    /// the number belongs — signed by the requester, so it is their claim
    /// about what they accepted rather than anyone's later reconstruction.
    pub agreed_price: Amount,
    /// The oracle mid this price was derived from, for a floating
    /// advertisement. `None` for a fixed one, where there is nothing to
    /// derive and the merchant's own signed price is the whole story.
    ///
    /// Recorded so the arithmetic is checkable by every node without any
    /// of them having to agree about the oracle — see
    /// `PricingModel::agrees_with`. Whether the mid itself was honest is a
    /// dispute question, and an arbitrator can answer it against the
    /// oracle records, which are replicated and timestamped.
    pub agreed_mid: Option<f64>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedReservationRequest {
    pub request: ReservationRequest,
    pub signature: Signature,
}

impl SignedReservationRequest {
    pub fn sign(request: ReservationRequest, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&request)
            .expect("ReservationRequest always serializes");
        Self {
            signature: keypair.sign(&bytes),
            request,
        }
    }

    pub fn verify(&self) -> Result<(), ReservationError> {
        let expected = peer_id_from_public_key(&self.request.requester_public_key)
            .map_err(|_| ReservationError::InvalidSignature)?;
        if expected != self.request.requester {
            return Err(ReservationError::UnauthorizedUpdate);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.request)
            .map_err(|_| ReservationError::MalformedReservation)?;
        verify(&self.request.requester_public_key, &bytes, &self.signature)
            .map_err(|_| ReservationError::InvalidSignature)
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
        let bytes = openfiat_serialization::json::to_bytes(&cancel)
            .expect("ReservationCancel always serializes");
        Self {
            signature: keypair.sign(&bytes),
            cancel,
        }
    }
}
