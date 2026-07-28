//! The signed-request envelope backing Phase 7's planned RPC auth model:
//! "signed-request pattern (wallet signature over a nonce/timestamp) for
//! anything that mutates state or reads account-scoped data — no
//! separate API-key system, reuses the same keys every other OpenFiat
//! signature already uses."
//!
//! `nonce` is carried so a consuming service (the future RPC layer) can
//! reject an exact repeat outright; this crate only defines the shape
//! and the freshness check, since deduplicating nonces needs a store
//! that belongs to whatever service is doing the authenticating.

use crate::error::WalletError;
use crate::wallet::Wallet;
use openfiat_crypto::verify;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_serialization::wire;
use openfiat_types::{PeerId, PublicKey, Signature, Timestamp};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RequestEnvelope<T> {
    pub payload: T,
    pub wallet: PeerId,
    pub wallet_public_key: PublicKey,
    pub nonce: u64,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedRequest<T> {
    pub envelope: RequestEnvelope<T>,
    pub signature: Signature,
}

impl Wallet {
    pub fn sign_request<T: serde::Serialize>(&self, payload: T, nonce: u64) -> Result<SignedRequest<T>, WalletError> {
        let envelope = RequestEnvelope { payload, wallet: self.peer_id(), wallet_public_key: self.public_key(), nonce, timestamp: Timestamp::now() };
        let bytes = wire::to_bytes(&envelope).map_err(|_| WalletError::MalformedRequest)?;
        Ok(SignedRequest { signature: self.sign(&bytes), envelope })
    }
}

/// Verifies a signed request's self-consistency (the embedded public key
/// really derives the claimed wallet), its signature, and that its
/// timestamp falls within `max_age` of `now` — the same freshness check
/// every gossip-transport crate performs on its own signed events,
/// applied here to a generic authenticated request instead.
pub fn verify_request<T: serde::Serialize>(signed: &SignedRequest<T>, now: Timestamp, max_age: Duration) -> Result<(), WalletError> {
    let expected = peer_id_from_public_key(&signed.envelope.wallet_public_key).map_err(|_| WalletError::InvalidSignature)?;
    if expected != signed.envelope.wallet {
        return Err(WalletError::InvalidSignature);
    }
    let bytes = wire::to_bytes(&signed.envelope).map_err(|_| WalletError::MalformedRequest)?;
    verify(&signed.envelope.wallet_public_key, &bytes, &signed.signature).map_err(|_| WalletError::InvalidSignature)?;

    let age_ms = now.since(signed.envelope.timestamp).ok_or(WalletError::RequestExpired)?;
    if age_ms > max_age.as_millis() as u64 {
        return Err(WalletError::RequestExpired);
    }
    Ok(())
}
