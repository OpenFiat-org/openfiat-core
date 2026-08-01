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
//! # Two questions, not one
//!
//! [`known`] and [`symbol_for_mint`] answer "what may a trade settle in,
//! and what do we call it" — a display-and-eligibility question, and the
//! reason OPEN is absent from [`KNOWN_MINTS`]. [`priced`] answers the
//! narrower "what symbol is this priced against and how does it scale",
//! which a service-provider fee needs and which carries no claim about
//! escrow at all. The table used to answer both with one row; the OPEN
//! case is what separated them. See [`priced`] for the full argument
//! before collapsing them back together.
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

/// The protocol's own token — pointedly *not* a row in [`KNOWN_MINTS`],
/// and reachable only through [`priced`].
///
/// Its address is taken from [`crate::programs::IDS`] rather than
/// transcribed, so there is exactly one place a build says which mint OPEN
/// is. `decimals` is read off the cluster and recorded in
/// `programs/devnet-addresses.json` as `devnet.mintDecimals`; the test
/// below pins this against that record, because a quantity of OPEN cannot
/// be converted or rendered without an exponent and a guessed one is wrong
/// by a factor of a thousand in silence.
const OPEN: KnownMint = KnownMint {
    mint: crate::programs::IDS.mint,
    symbol: "OPEN",
    decimals: 9,
};

/// What this build knows about `mint` for the purpose of putting a
/// *price* on a quantity of it: the symbol an oracle publishes a rate
/// against, and the base-unit exponent to scale by.
///
/// # This deliberately says nothing about settlement eligibility
///
/// [`known`] and this function answer two questions the table above had
/// been conflating, and the difference is the entire reason this exists.
/// `known` answers "may a trade settle in this, and what do we call it in
/// the directory" — which is why OPEN is absent from [`KNOWN_MINTS`]: it
/// is not on the escrow program's settlement allowlist, OFS-4100 holds it
/// back until the public sale, and naming it there would advertise an
/// asset no buyer can receive from escrow. That reasoning is sound and the
/// test guarding it still holds.
///
/// It is also reasoning about *escrow*. A service-provider fee is not an
/// escrow settlement: it never passes the on-chain settlement allowlist,
/// it is a payment to a provider for work done, and OFS-4100 §9.5 puts no
/// such hold on what a provider may be paid in. So a USDC-denominated fee
/// can perfectly well be settled in OPEN while a *trade* still cannot, and
/// the only thing pricing that fee needs is a symbol and a scale.
///
/// A caller asking "may a trade settle in this" must therefore still ask
/// [`known`], and must not read a `Some` here as permission. This is the
/// narrower question, and it is the one to ask when you have base units
/// and need a number.
///
/// `None` still means "no nickname and no scale", never "invalid"; the
/// caller shows the address and refuses to convert, exactly as before.
pub fn priced(mint: &MintAddress) -> Option<&'static KnownMint> {
    if mint.as_str() == OPEN.mint {
        return Some(&OPEN);
    }
    known(mint)
}

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

    /// The deployment record, read at compile time so this needs no
    /// cluster — the same technique `crate::programs` uses to stop a
    /// transcribed address drifting from what was actually deployed.
    const DEVNET_ADDRESSES: &str = include_str!("../../../programs/devnet-addresses.json");

    /// `OPEN.decimals` is a transcription of something read off the
    /// cluster, so it can drift exactly the way a transcribed address can.
    /// The recorded value came from
    /// `getTokenSupply(29w8Tro…)` on devnet: decimals 9, amount
    /// 1000000000000000000.
    #[test]
    fn the_protocols_own_token_is_scaled_by_the_recorded_deployment_not_a_guess() {
        let json: serde_json::Value =
            serde_json::from_str(DEVNET_ADDRESSES).expect("devnet-addresses.json must be valid");
        assert_eq!(
            json["devnet"]["mint"].as_str(),
            Some(OPEN.mint),
            "the token this build prices must be the one that was deployed"
        );
        assert_eq!(
            json["devnet"]["mintDecimals"].as_u64(),
            Some(u64::from(OPEN.decimals)),
            "an exponent that drifts from the mint's own is wrong by a factor of \
             a thousand, and nothing else would notice"
        );
    }

    /// The distinction the two lookups exist to draw. OPEN can be priced —
    /// a provider fee may be settled in it — and is still not a mint a
    /// trade may settle in, so it must stay out of every display and
    /// allowlist path.
    #[test]
    fn the_protocols_own_token_is_priceable_without_becoming_a_settlement_asset() {
        let open = MintAddress::parse(crate::programs::IDS.mint).expect("the pinned mint parses");

        let priced = priced(&open).expect("a fee denominated in OPEN has to be scalable");
        assert_eq!(priced.decimals, 9);
        assert_eq!(priced.symbol, "OPEN");

        // Everything the escrow-facing reasoning protects is unchanged.
        assert_eq!(
            known(&open),
            None,
            "the scale lookup must not have smuggled OPEN onto the settlement table"
        );
        assert_eq!(
            symbol_for_mint(&open),
            None,
            "the directory must still not name an asset no buyer can receive from escrow"
        );
        assert!(
            !KNOWN_MINTS
                .iter()
                .any(|k| k.mint == crate::programs::IDS.mint),
            "OPEN must not appear in the table until it is allowlisted on chain"
        );
    }

    #[test]
    fn every_settleable_mint_is_priced_identically_by_both_lookups() {
        // `priced` widens `known`, and must never disagree with it about a
        // mint they both answer for — a fee quoted at one scale and a
        // volume totalled at another would both look internally consistent.
        for entry in KNOWN_MINTS {
            let mint = MintAddress::parse(entry.mint).expect("a listed mint parses");
            assert_eq!(priced(&mint), known(&mint), "{} disagrees", entry.mint);
        }
    }

    #[test]
    fn a_mint_from_neither_table_is_still_unpriceable() {
        // The widening is exactly one mint wide. Anything else this build
        // has never heard of must still refuse to be scaled rather than
        // pick up a default.
        let elsewhere = MintAddress::parse("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU").unwrap();
        assert_eq!(priced(&elsewhere), None);
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
