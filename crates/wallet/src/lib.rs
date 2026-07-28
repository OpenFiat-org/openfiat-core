//! `openfiat-wallet` — Wallet key management and transaction signing abstractions.
//!
//! No OFS spec of its own — this crate exists to give the pieces of this
//! workspace that need a wallet identity a single shared implementation:
//! the Phase 7 RPC signed-request auth model (see the `request` module
//! doc), loading a node's identity from a Solana-format wallet.json (see
//! `solana_keyfile`), and, later, Solana staking/governance instruction
//! builders that need the same keypair to sign transactions with.

pub mod error;
pub mod request;
pub mod solana_keyfile;
pub mod wallet;

pub use error::WalletError;
pub use request::{RequestEnvelope, SignedRequest, verify_request};
pub use solana_keyfile::KeyfileError;
pub use wallet::Wallet;

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_types::Timestamp;
    use std::time::Duration;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }

    #[test]
    fn a_signed_request_round_trips() {
        let wallet = Wallet::generate();
        let signed = wallet.sign_request("do the thing".to_string(), 1).unwrap();
        assert_eq!(
            verify_request(&signed, Timestamp::now(), Duration::from_secs(30)),
            Ok(())
        );
    }

    #[test]
    fn a_request_signed_by_a_different_wallet_is_rejected() {
        let wallet = Wallet::generate();
        let attacker = Wallet::generate();
        let mut signed = wallet.sign_request("do the thing".to_string(), 1).unwrap();
        signed.signature = attacker.sign(b"forged");
        assert_eq!(
            verify_request(&signed, Timestamp::now(), Duration::from_secs(30)),
            Err(WalletError::InvalidSignature)
        );
    }

    #[test]
    fn a_stale_request_is_rejected() {
        let wallet = Wallet::generate();
        let signed = wallet.sign_request("do the thing".to_string(), 1).unwrap();
        let far_future = Timestamp::from_millis(
            signed.envelope.timestamp.as_millis() + Duration::from_secs(60).as_millis() as u64,
        );
        assert_eq!(
            verify_request(&signed, far_future, Duration::from_secs(30)),
            Err(WalletError::RequestExpired)
        );
    }

    #[test]
    fn a_request_whose_claimed_wallet_does_not_match_its_key_is_rejected() {
        let wallet = Wallet::generate();
        let other = Wallet::generate();
        let mut signed = wallet.sign_request("do the thing".to_string(), 1).unwrap();
        signed.envelope.wallet = other.peer_id();
        assert_eq!(
            verify_request(&signed, Timestamp::now(), Duration::from_secs(30)),
            Err(WalletError::InvalidSignature)
        );
    }
}
