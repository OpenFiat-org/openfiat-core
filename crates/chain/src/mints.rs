//! Which mint is which, for the places a person has to read one.
//!
//! # This is a display table, not an allowlist
//!
//! The settlement-mint allowlist lives on chain, in `openfiat-escrow`'s
//! `FeeConfig`, and is governance-updatable. It is the enforcement: a mint
//! that is not on it cannot back a liquidity vault, cannot be reserved
//! against, and cannot fund a trade escrow.
//!
//! This module deliberately enforces nothing. It answers one question —
//! "what do people call this address" — so an interface can show `USDC`
//! instead of `2bHPi5hA4z…` without the merchant getting to choose the
//! label. A node that rejected an advertisement because its mint was
//! absent from *this* list would be enforcing a stale copy of a rule it is
//! not the authority for: governance adds a mint on Tuesday, and every
//! node built before Tuesday starts refusing legitimate trades. Two honest
//! nodes on different releases would disagree about which advertisements
//! are valid, which is the test `programs`'s own doc sets for what may and
//! may not be a constant.
//!
//! So an unknown mint is not an error. It is an address with no nickname,
//! and the honest thing to show is the address.
//!
//! # Why a symbol may never travel in a record
//!
//! A ticker is spoofable and cluster-dependent: "USDC" names one mint on
//! mainnet, a different one on devnet, and anything at all in a string
//! field somebody else filled in. An address is neither. That is why
//! `openfiat_advertisements::Advertisement` carries a
//! [`openfiat_crypto::MintAddress`] and no asset name, and why the name
//! is resolved here, at the edge, from a table every node compiles in
//! identically.

use openfiat_crypto::MintAddress;

/// One mint a person might see named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownMint {
    /// Base58 address — the identity.
    pub mint: &'static str,
    /// What to call it. Not authoritative for anything; see the module doc.
    pub symbol: &'static str,
    /// Base-unit exponent, so an interface can render an `Amount` without
    /// asking a node that may not have the mint account cached.
    pub decimals: u8,
}

/// The mints this build knows the names of, transcribed from
/// `programs/programs/escrow/src/constants.rs`'s `DEFAULT_SETTLEMENT_MINTS`
/// — the list the escrow program actually ships with. The test below
/// re-reads that file, so a mint added there and forgotten here shows up
/// as a failing build rather than as an address with no name in the UI.
///
/// Entries 2–4 are **devnet-only**. On mainnet each would be a look-alike
/// of the asset it is named after, which is the precise failure the
/// on-chain allowlist exists to refuse; a mainnet deployment replaces them
/// wholesale, in the same commit that replaces the on-chain list.
pub const DEVNET: &[KnownMint] = &[
    KnownMint {
        mint: "So11111111111111111111111111111111111111112",
        symbol: "wSOL",
        decimals: 9,
    },
    KnownMint {
        mint: "2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU",
        symbol: "USDC",
        decimals: 6,
    },
    KnownMint {
        mint: "C4rSGhdxWhSFQuFcAxQti1JvBxriwHJoHtJjfhs5p24Y",
        symbol: "USDT",
        decimals: 6,
    },
    KnownMint {
        // The Token-2022 entry, and the one the running devnet deployment
        // denominates its fee treasuries in — see the escrow constants for
        // why de-listing it would cut the deployment off from its own fee
        // collection.
        mint: "SK1JEbfsjjTG2WELNirmM7iJVcdnwerqfF32kCnoWsM",
        symbol: "tUSDC",
        decimals: 6,
    },
];

// OPEN is deliberately absent, and the first draft of this table got it
// wrong. `crate::programs::IDS.mint` is the protocol's own token, but it
// is **not** on the escrow settlement allowlist — OFS-4100 holds it back
// until the public sale — so naming it here would have put a familiar
// label on something no buyer can receive from escrow. That is the same
// class of lie this whole change removes, pointed the other way, and it
// is exactly what the test below exists to catch.

/// The table this build uses. Selected at compile time alongside
/// [`crate::programs::IDS`], never by configuration.
pub const KNOWN_MINTS: &[KnownMint] = DEVNET;

/// What people call `mint`, if this build knows.
///
/// `None` means "no nickname", not "invalid". The caller shows the address.
pub fn symbol_for_mint(mint: &MintAddress) -> Option<&'static str> {
    KNOWN_MINTS
        .iter()
        .find(|known| known.mint == mint.as_str())
        .map(|known| known.symbol)
}

/// Everything this build knows about `mint` — its name *and* its scale.
///
/// [`symbol_for_mint`] answers the question a label asks. This answers the
/// question a *quantity* asks, and the two are not the same call because
/// the second one can be wrong in a way the first cannot: a caller that has
/// base units and guesses the exponent prints a number off by a factor of
/// a thousand, silently and plausibly. `decimals` is therefore taken from
/// the same row as the symbol, so a build cannot know what to call a mint
/// while assuming how to scale it.
///
/// `None` carries the same meaning as it does in [`symbol_for_mint`]: no
/// nickname, not invalid. A caller totalling base units can still total
/// them; what it must not do is decide where the decimal point goes.
pub fn known(mint: &MintAddress) -> Option<&'static KnownMint> {
    KNOWN_MINTS.iter().find(|known| known.mint == mint.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The escrow program's own list, read at compile time so this check
    /// needs no cluster.
    const ESCROW_CONSTANTS: &str =
        include_str!("../../../programs/programs/escrow/src/constants.rs");

    #[test]
    fn every_named_mint_is_one_the_escrow_program_ships_allowlisted() {
        // The direction that matters. A name here for a mint the program
        // will not settle is a label on something a buyer can never
        // receive — which is the same class of lie this whole change
        // exists to remove, pointed the other way.
        for known in KNOWN_MINTS {
            assert!(
                ESCROW_CONSTANTS.contains(known.mint),
                "{} ({}) is named here but absent from DEFAULT_SETTLEMENT_MINTS",
                known.symbol,
                known.mint
            );
        }
    }

    #[test]
    fn the_protocols_own_token_is_not_named_as_a_settlement_asset() {
        // OPEN is not on the escrow allowlist until the public sale, so a
        // trade cannot settle in it. Naming it here would advertise an
        // asset a buyer cannot be paid in — and being the protocol's own
        // token is precisely what would make that label convincing.
        assert!(
            !KNOWN_MINTS
                .iter()
                .any(|k| k.mint == crate::programs::IDS.mint),
            "OPEN must not appear until it is allowlisted on chain"
        );
    }

    #[test]
    fn no_two_entries_claim_the_same_name_or_the_same_address() {
        // Two mints called USDC would make `symbol_for` answer by
        // declaration order, which is not an answer.
        for (i, a) in KNOWN_MINTS.iter().enumerate() {
            for b in &KNOWN_MINTS[i + 1..] {
                assert_ne!(a.symbol, b.symbol, "two mints named {}", a.symbol);
                assert_ne!(a.mint, b.mint, "{} listed twice", a.mint);
            }
        }
    }

    #[test]
    fn the_name_lookup_and_the_scale_lookup_never_disagree_about_a_mint() {
        // Two functions read this one table, and callers mix them: the
        // advertisement view names a mint with `symbol_for_mint` while
        // `getSettledVolume` takes name *and* decimals from `known`. If
        // they ever answered differently, one response could name a mint
        // that another said nothing about, and no test above would notice
        // — both would still be internally consistent.
        for entry in KNOWN_MINTS {
            let mint = MintAddress::parse(entry.mint).expect("a listed mint parses");
            assert_eq!(
                symbol_for_mint(&mint),
                known(&mint).map(|k| k.symbol),
                "{} is named differently by the two lookups",
                entry.mint
            );
        }
    }

    #[test]
    fn a_scale_is_never_left_to_a_caller_to_guess() {
        // Decimals ride with the symbol precisely so a caller cannot know
        // what to call a mint while assuming how to scale it. wSOL is 9
        // and the stablecoins are 6, so a build that defaulted to 6 would
        // report SOL volume a thousand times too large — the failure
        // `getSettledVolume`'s own doc calls out.
        for entry in KNOWN_MINTS {
            let mint = MintAddress::parse(entry.mint).expect("a listed mint parses");
            assert_eq!(known(&mint).map(|k| k.decimals), Some(entry.decimals));
        }
    }

    #[test]
    fn an_unknown_mint_has_no_nickname_rather_than_a_wrong_one() {
        // Circle's canonical devnet USDC, which this deployment
        // deliberately does not use — see the escrow constants for why.
        // It must come back nameless, not as "USDC".
        let elsewhere = MintAddress::parse("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU").unwrap();
        assert_eq!(symbol_for_mint(&elsewhere), None);
    }
}
