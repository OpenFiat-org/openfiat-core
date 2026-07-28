//! Ed25519 signature verification.

use ed25519_dalek::{Verifier, VerifyingKey};
use openfiat_types::{ErrorCode, PublicKey, Signature};
use std::fmt;

/// A signature failed to verify, was malformed, or the public key was invalid.
///
/// Deliberately collapses every failure mode into one variant: distinguishing
/// "malformed input" from "valid input, wrong signature" to a caller would
/// invite exactly the kind of oracle a signature check must not leak.
#[derive(Debug)]
pub struct VerifyError;

impl VerifyError {
    /// The OFS-8000 code this failure maps to (`INVALID_SIGNATURE`, 1003).
    pub const fn code(&self) -> ErrorCode {
        ErrorCode::InvalidSignature
    }
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "signature verification failed")
    }
}

impl std::error::Error for VerifyError {}

/// Verify that `signature` over `message` was produced by the holder of
/// `public_key`'s private key.
pub fn verify(public_key: &PublicKey, message: &[u8], signature: &Signature) -> Result<(), VerifyError> {
    let verifying_key = VerifyingKey::from_bytes(public_key.as_bytes()).map_err(|_| VerifyError)?;
    let signature_bytes = signature.as_bytes().ok_or(VerifyError)?;
    let signature = ed25519_dalek::Signature::from_bytes(&signature_bytes);
    verifying_key.verify(message, &signature).map_err(|_| VerifyError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keypair::Keypair;

    #[test]
    fn accepts_a_genuine_signature() {
        let keypair = Keypair::generate();
        let signature = keypair.sign(b"hello openfiat");
        assert!(verify(&keypair.public_key(), b"hello openfiat", &signature).is_ok());
    }

    #[test]
    fn rejects_a_tampered_message() {
        let keypair = Keypair::generate();
        let signature = keypair.sign(b"hello openfiat");
        assert!(verify(&keypair.public_key(), b"goodbye openfiat", &signature).is_err());
    }

    #[test]
    fn rejects_a_signature_from_a_different_key() {
        let signer = Keypair::generate();
        let other = Keypair::generate();
        let signature = signer.sign(b"hello openfiat");
        assert!(verify(&other.public_key(), b"hello openfiat", &signature).is_err());
    }

    #[test]
    fn rejects_a_malformed_signature_without_panicking() {
        let keypair = Keypair::generate();
        let malformed = Signature::from_bytes([0u8; 64]);
        // Well-formed length, wrong content — must fail verification, not panic.
        let err = verify(&keypair.public_key(), b"hello openfiat", &malformed).unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidSignature);
    }
}
