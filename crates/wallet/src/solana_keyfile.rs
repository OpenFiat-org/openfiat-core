//! Loads (and saves) a Solana CLI-style wallet keypair file — the same
//! format `solana-keygen new` produces, conventionally at
//! `~/.config/solana/id.json`: a JSON array of 64 bytes, the
//! concatenation of a 32-byte Ed25519 seed and its 32-byte public key.
//!
//! OpenFiat node identity is already an Ed25519 keypair, the same as a
//! Solana wallet — so an operator who already runs Solana tooling can
//! point a node straight at their existing wallet file instead of
//! managing a second, separate identity.

use crate::wallet::Wallet;
use std::fmt;
use std::path::Path;

#[derive(Debug)]
pub enum KeyfileError {
    Read(std::io::Error),
    Write(std::io::Error),
    Parse(serde_json::Error),
    /// A well-formed JSON array, but not the 64 bytes a Solana keypair
    /// file always is.
    WrongLength(usize),
    /// The file's embedded public key (bytes 32..64) doesn't match what
    /// its own seed (bytes 0..32) actually derives — a truncated or
    /// hand-edited file, caught here rather than silently deriving the
    /// wrong node identity.
    PublicKeyMismatch,
}

impl fmt::Display for KeyfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => write!(f, "failed to read keyfile: {e}"),
            Self::Write(e) => write!(f, "failed to write keyfile: {e}"),
            Self::Parse(e) => write!(f, "failed to parse keyfile as a JSON byte array: {e}"),
            Self::WrongLength(len) => write!(
                f,
                "keyfile must contain exactly 64 bytes (seed + public key), found {len}"
            ),
            Self::PublicKeyMismatch => write!(
                f,
                "keyfile's embedded public key does not match its own seed"
            ),
        }
    }
}

impl std::error::Error for KeyfileError {}

/// Loads a wallet from a Solana-format keypair file, verifying the
/// embedded public key against the seed's own derivation.
pub fn load(path: impl AsRef<Path>) -> Result<Wallet, KeyfileError> {
    let contents = std::fs::read_to_string(path).map_err(KeyfileError::Read)?;
    let bytes: Vec<u8> = serde_json::from_str(&contents).map_err(KeyfileError::Parse)?;
    if bytes.len() != 64 {
        return Err(KeyfileError::WrongLength(bytes.len()));
    }

    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes[..32]);
    let wallet = Wallet::from_seed(seed);
    if wallet.public_key().as_bytes().as_slice() != &bytes[32..64] {
        return Err(KeyfileError::PublicKeyMismatch);
    }
    Ok(wallet)
}

/// Saves `wallet` in the same 64-byte Solana keypair format `load`
/// reads, so a node identity created here is equally usable as a Solana
/// CLI wallet and vice versa.
pub fn save(wallet: &Wallet, path: impl AsRef<Path>) -> Result<(), KeyfileError> {
    let mut bytes = wallet.seed().to_vec();
    bytes.extend_from_slice(wallet.public_key().as_bytes().as_slice());
    let json = serde_json::to_string(&bytes).map_err(KeyfileError::Parse)?;
    std::fs::write(path, json).map_err(KeyfileError::Write)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_wallet_loads_back_to_the_same_identity() {
        let wallet = Wallet::generate();
        let path =
            std::env::temp_dir().join(format!("openfiat-wallet-test-{}.json", std::process::id()));
        save(&wallet, &path).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.peer_id(), wallet.peer_id());
        assert_eq!(loaded.public_key(), wallet.public_key());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_file_that_is_not_64_bytes_is_rejected() {
        let path = std::env::temp_dir().join(format!(
            "openfiat-wallet-test-short-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, "[1,2,3]").unwrap();

        let result = load(&path);
        assert!(matches!(result, Err(KeyfileError::WrongLength(3))));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_tampered_public_key_is_rejected() {
        let wallet = Wallet::generate();
        let path = std::env::temp_dir().join(format!(
            "openfiat-wallet-test-tampered-{}.json",
            std::process::id()
        ));
        save(&wallet, &path).unwrap();

        let mut bytes: Vec<u8> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        bytes[63] ^= 0xFF;
        std::fs::write(&path, serde_json::to_string(&bytes).unwrap()).unwrap();

        assert!(matches!(load(&path), Err(KeyfileError::PublicKeyMismatch)));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_missing_file_reports_a_read_error() {
        let result = load("/nonexistent/path/does-not-exist.json");
        assert!(matches!(result, Err(KeyfileError::Read(_))));
    }
}
