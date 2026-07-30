//! Protocol identity: the on-chain program ids and token mint this build
//! of the node is pinned to (OFS-4200's programs, reached over OFS-4300).
//!
//! # Why these are constants and not configuration
//!
//! A program id is not a setting, it is part of the protocol's identity —
//! the same class of value as a PDA seed or an Anchor discriminator. It is
//! what makes an account's *owner* meaningful: `actor::poll_vote_verifications`
//! decides whether a governance vote's claimed stake is genuine by checking
//! that the account it read is owned by **the** staking program. If a node
//! operator could name that program themselves, they could deploy their own
//! staking program, mint `StakeAccount`s holding any balance they liked, and
//! their node would count governance votes weighted by stake that does not
//! exist. The verification would still run; it would simply be verifying
//! against an authority the attacker controls.
//!
//! This node is distributed as open source and run by people the protocol
//! does not trust individually, so the test that decides whether a value may
//! be configured is: *if two honest nodes running the same release could
//! disagree because of it, it is not configuration.* Program ids fail that
//! test outright. Contrast the values that pass it — bind addresses, the
//! ledger directory, entry points, Solana RPC endpoints — which are
//! `openfiat-node`'s command-line flags, and the protocol *parameters*
//! (fees, stake minimums, quorum), which are neither constants here nor
//! operator settings but on-chain state under governance control.
//!
//! `openfiat-core/programs/shared` already treats this class of value the
//! same way, pinning `GOVERNANCE_PROGRAM_ID` as a `pubkey!` constant so the
//! ban-list gate cannot be pointed at a program that never writes ban
//! records. This module is the off-chain half of that decision.
//!
//! # Adding a network
//!
//! Only devnet is deployed, so [`IDS`] is [`DEVNET`] unconditionally and
//! there is no network-selection feature. Adding mainnet means deploying
//! the programs, recording the real ids, defining a `MAINNET: ProgramIds`
//! beside [`DEVNET`], and introducing the feature that selects between
//! them at that point.
//!
//! Do not add the feature ahead of the ids. An earlier version declared
//! `network-mainnet` with a `compile_error!` behind it, reasoning that a
//! mainnet build should fail rather than silently inherit devnet's ids.
//! The reasoning was right and the mechanism was wrong: `cargo` cannot
//! exclude a feature from `--all-features`, which CI runs, so a feature
//! that can only fail to compile fails the build for everyone. Having no
//! feature is stricter anyway — a mainnet build cannot be requested at
//! all. Placeholder ids remain the worst option of the three: they would
//! look real.
//!
//! There is deliberately no runtime escape hatch, not even for a local test
//! validator. Anchor deploys from a fixed program keypair (`declare_id!`),
//! so a locally deployed program has the *same* id as the devnet
//! deployment; a local cluster needs a different RPC endpoint, which is
//! already operator-settable, not a different program id.

use solana_pubkey::Pubkey;

/// One deployment's protocol identity — every address that identifies
/// *the* OpenFiat programs rather than any particular node's setup.
///
/// Held as base58 `&'static str` rather than [`Pubkey`], because the only
/// consumer compares against `ChainClient::get_account`'s owner field,
/// which is already a canonically-encoded base58 string; each id is still
/// proved to be a real 32-byte address at compile time (see the `const _`
/// assertions below `DEVNET`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramIds {
    /// Cluster name, for diagnostics — a node should be able to say which
    /// deployment it was built against without the operator guessing.
    pub network: &'static str,
    /// `openfiat-staking` (OFS-4200 §5). The authority a governance vote's
    /// claimed `StakeAccount` must be owned by before its decoded weight
    /// is trusted.
    pub staking: &'static str,
    /// `openfiat-governance` (OFS-4200 §6) — also the owner of the
    /// OFS-7100 §12 ban records.
    pub governance: &'static str,
    /// `openfiat-escrow` (OFS-4200 §4), the trade escrow vaults a
    /// settlement's on-chain release moves funds out of.
    pub escrow: &'static str,
    /// `openfiat-presale`, the OPEN token sale program.
    pub presale: &'static str,
    /// The OPEN token mint (Token-2022). Genesis identity: every vault,
    /// treasury and stake account in the protocol is denominated in it.
    pub mint: &'static str,
}

/// The devnet deployment, transcribed from
/// `openfiat-core/programs/devnet-addresses.json` — which is the record of
/// what was actually deployed, and which this module's own test re-reads
/// so a typo here fails the build rather than silently rejecting every
/// real vote.
pub const DEVNET: ProgramIds = ProgramIds {
    network: "devnet",
    staking: "HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx",
    governance: "AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE",
    escrow: "HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP",
    presale: "75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7",
    mint: "29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj",
};

// Compile-time proof that each id above is a real base58-encoded 32-byte
// address: `Pubkey::from_str_const` panics on anything else, and a panic
// in a `const` initializer is a build failure, not a runtime surprise. A
// transposed or truncated character therefore cannot reach a release.
const _: Pubkey = Pubkey::from_str_const(DEVNET.staking);
const _: Pubkey = Pubkey::from_str_const(DEVNET.governance);
const _: Pubkey = Pubkey::from_str_const(DEVNET.escrow);
const _: Pubkey = Pubkey::from_str_const(DEVNET.presale);
const _: Pubkey = Pubkey::from_str_const(DEVNET.mint);

/// The deployment this binary is built against. Selected at compile time,
/// by cargo feature — never by environment, file, or RPC parameter.
pub const IDS: ProgramIds = DEVNET;

#[cfg(test)]
mod tests {
    use super::*;

    /// The deployment record this module transcribes. Read at compile time
    /// so the check needs no cluster and no network access — a typo in a
    /// constant above fails CI here, instead of surviving to production
    /// where its only symptom is that every genuine vote is rejected for
    /// having the "wrong" owner program.
    const DEVNET_ADDRESSES: &str = include_str!("../../../programs/devnet-addresses.json");

    fn deployed(section: &str, key: &str) -> String {
        let json: serde_json::Value =
            serde_json::from_str(DEVNET_ADDRESSES).expect("devnet-addresses.json must be valid");
        json[section][key]
            .as_str()
            .unwrap_or_else(|| panic!("devnet-addresses.json is missing {section}.{key}"))
            .to_string()
    }

    #[test]
    fn pinned_ids_match_the_recorded_devnet_deployment() {
        assert_eq!(DEVNET.staking, deployed("devnet_programs", "staking"));
        assert_eq!(DEVNET.governance, deployed("devnet_programs", "governance"));
        assert_eq!(DEVNET.escrow, deployed("devnet_programs", "escrow"));
        assert_eq!(DEVNET.presale, deployed("devnet_sale", "programId"));
        assert_eq!(DEVNET.mint, deployed("devnet", "mint"));
    }

    /// The mint is recorded in three places by three different deploy
    /// steps; if they ever disagree, the one pinned here is ambiguous.
    #[test]
    fn the_recorded_mint_agrees_with_itself_across_sections() {
        assert_eq!(
            deployed("devnet", "mint"),
            deployed("devnet_programs", "mint")
        );
        assert_eq!(
            deployed("devnet", "mint"),
            deployed("devnet_sale", "openMint")
        );
    }

    #[test]
    fn the_selected_deployment_is_devnet() {
        assert_eq!(IDS, DEVNET);
        assert_eq!(IDS.network, "devnet");
    }
}
