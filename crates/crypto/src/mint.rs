//! A validated Solana mint address.
//!
//! # Why an advertisement names a mint rather than a ticker
//!
//! An advertisement used to carry `asset: String` — free-form text, chosen
//! by the merchant, displayed to the buyer as the asset they were about to
//! receive. Nothing connected that string to the token the escrow would
//! actually move. A merchant could advertise "USDC" and settle in
//! something else, and every layer would agree the trade completed
//! correctly, because each one did exactly what it was asked.
//!
//! The on-chain allowlist (`openfiat-escrow`'s `FeeConfig`) closed the
//! worst version of that: a mint the merchant created themselves can no
//! longer be escrowed at all. What it cannot close is the gap between a
//! *label* and an *identity*, because the program never sees the label.
//!
//! So the label goes. An advertisement names the mint, and a symbol shown
//! to a buyer is derived from the mint rather than supplied alongside it.
//! `openfiat_registry`'s `ServicePricing.token_mint` already reached this
//! conclusion for provider fees, on the same reasoning: a symbol is
//! ambiguous across clusters and spoofable, a mint address is neither.
//!
//! # What this type does and does not check
//!
//! It checks that the value is a real address: base58, decoding to exactly
//! 32 bytes. It deliberately does **not** check membership of any list.
//!
//! That restraint is the point. The settlement-mint allowlist lives on
//! chain and is governance-updatable, so a node built last month must not
//! reject an advertisement naming a mint governance approved last week —
//! it would be enforcing a stale copy of a rule it is not the authority
//! for, and two honest nodes on different releases would disagree about
//! which advertisements are valid. Enforcement belongs where the funds
//! move. This type's job is to make the field unambiguous.

/// The only way a mint address can be wrong here: it is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintError {
    Malformed,
}

impl std::fmt::Display for MintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not a base58-encoded 32-byte Solana address")
    }
}

impl std::error::Error for MintError {}

/// A base58 Solana address, proven to decode to 32 bytes.
///
/// Construct with [`MintAddress::parse`]. No `From<String>`, no public
/// field, and a `Deserialize` that goes through the parser — a mint
/// arriving in a gossiped record has crossed a trust boundary exactly like
/// one typed by a merchant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct MintAddress(String);

/// A Solana address is a 32-byte Ed25519 public key or program-derived
/// address; base58 is only its spelling.
const ADDRESS_BYTES: usize = 32;

impl MintAddress {
    pub fn parse(input: &str) -> Result<Self, MintError> {
        // Length-bounded before decoding: base58 of 32 bytes is 32–44
        // characters, and refusing longer input up front means a very long
        // string cannot make the decoder do arbitrary work.
        if input.len() < 32 || input.len() > 44 {
            return Err(MintError::Malformed);
        }
        let decoded = bs58::decode(input)
            .into_vec()
            .map_err(|_| MintError::Malformed)?;
        if decoded.len() != ADDRESS_BYTES {
            return Err(MintError::Malformed);
        }
        // The input string is kept rather than re-encoded from the bytes.
        // base58 has no alternative spellings for the same bytes — unlike
        // base32 with its padding, or hex with its case — so the value
        // that arrived is already canonical, and round-tripping it would
        // only introduce a way for storage and display to disagree.
        Ok(Self(input.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MintAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for MintAddress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        MintAddress::parse(&raw)
            .map_err(|_| serde::de::Error::custom("not a base58 32-byte Solana address"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrapped SOL — cluster-independent, and the first entry in the
    /// escrow program's own settlement allowlist.
    const WSOL: &str = "So11111111111111111111111111111111111111112";
    /// The devnet mock USDC this project actually minted and allowlisted.
    const DEVNET_USDC: &str = "2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU";

    #[test]
    fn parses_the_mints_this_protocol_really_settles_in() {
        for real in [WSOL, DEVNET_USDC] {
            assert_eq!(
                MintAddress::parse(real).map(|m| m.as_str().to_string()),
                Ok(real.to_string())
            );
        }
    }

    #[test]
    fn a_ticker_is_not_an_address() {
        // The whole point. "USDC" is what a merchant used to be able to
        // put in this field, and it is exactly what must stop being
        // accepted where an identity is required.
        for label in ["USDC", "USDT", "usd-coin", ""] {
            assert_eq!(MintAddress::parse(label), Err(MintError::Malformed));
        }
    }

    #[test]
    fn a_string_of_legal_base58_that_is_the_wrong_length_is_refused() {
        // 31 bytes and 33 bytes both encode to plausible-looking base58.
        // Only a 32-byte decode is an address.
        let short = bs58::encode([7u8; 31]).into_string();
        let long = bs58::encode([7u8; 33]).into_string();
        assert_eq!(MintAddress::parse(&short), Err(MintError::Malformed));
        assert_eq!(MintAddress::parse(&long), Err(MintError::Malformed));
    }

    #[test]
    fn base58s_excluded_characters_are_refused() {
        // `0`, `O`, `I` and `l` are omitted from the alphabet precisely
        // because they are confusable — which makes them the characters a
        // look-alike address would be built from.
        let mut confusable = WSOL.to_string();
        confusable.replace_range(0..1, "0");
        assert_eq!(MintAddress::parse(&confusable), Err(MintError::Malformed));
    }

    #[test]
    fn deserialization_cannot_mint_an_unvalidated_address() {
        let valid: MintAddress =
            serde_json::from_str(&format!("\"{WSOL}\"")).expect("a real mint round-trips");
        assert_eq!(valid.as_str(), WSOL);

        let ticker: Result<MintAddress, _> = serde_json::from_str("\"USDC\"");
        assert!(
            ticker.is_err(),
            "a peer's gossip must not be able to introduce a mint that never \
             passed the parser — that is the entire attack"
        );
    }
}
