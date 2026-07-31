//! The join between an off-chain proposal and the on-chain one, and the
//! decoder for the chain's half of it.
//!
//! # The problem this exists to fix
//!
//! This crate holds proposals as gossiped, signed off-chain records keyed
//! by an author-chosen string. The `openfiat-governance` Solana program
//! holds `Proposal` accounts keyed by a `u64`. Nothing correlated the
//! two. An interface could therefore show "the" proposal and be showing
//! either record while implying the other, with no way to notice when
//! they disagreed — and they can disagree, because the chain tallies
//! stake-weighted votes under its own quorum and threshold rules while
//! this layer only ever holds the votes a particular node happened to
//! verify.
//!
//! That gap is why [`crate::store::GovernanceRegistry::apply_onchain_resolution`]
//! shipped without a caller. This module is the missing half.
//!
//! # The join key, and why it takes two claims
//!
//! Each side names the other, and *both* must agree before anything is
//! treated as linked:
//!
//! - the off-chain proposal carries [`crate::record::Proposal::onchain_proposal_id`],
//!   the `u64` it claims on chain, inside the `ProposalCreate` event its
//!   author signed at creation and which cannot be amended afterwards;
//! - the on-chain proposal carries `offchain_id_hash`, the SHA-256 of the
//!   off-chain id, written once by the program's `link_offchain_proposal`
//!   and never again.
//!
//! Either claim alone is exactly that — a claim. Anyone may create an
//! on-chain proposal naming an off-chain id they do not own, and anyone
//! may gossip an off-chain proposal naming an on-chain id they did not
//! create. Requiring both directions means a link can only exist where
//! two independently-authorized writes agree, and a reader holding only
//! one of them is told [`ChainAgreement::ClaimNotReciprocated`] rather
//! than shown a guess.
//!
//! # Why a hand-rolled decoder
//!
//! `openfiat-core/programs` is a deliberately separate Cargo/Anchor
//! workspace pinning its own Solana SDK versions, so this crate cannot
//! import `governance::Proposal` and let Borsh do the work — the same
//! arrangement, for the same reason, as `crates/rpc::onchain_stake` and
//! `crates/rpc::onchain_dispute`. The discriminator and offsets below are
//! transcribed from a real `anchor build`'s
//! `programs/target/idl/governance.json`, and this module's own test
//! re-reads that file and recomputes every offset, so a layout change
//! that nobody mirrored here is caught rather than silently decoding the
//! wrong bytes into a governance outcome. That file is generated and
//! git-ignored, so the check reads it at run time and reports itself
//! skipped when it is absent — see the test.

use crate::error::GovernanceError;
use crate::record::{Proposal, ProposalId, ProposalStatus};

/// `sha256("account:Proposal")[..8]`, as Anchor itself computed it —
/// taken verbatim from `programs/target/idl/governance.json`.
const PROPOSAL_DISCRIMINATOR: [u8; 8] = [26, 94, 189, 187, 116, 136, 53, 33];

/// The deployed `openfiat-governance` program.
///
/// A second copy of `openfiat_chain::PROGRAM_IDS.governance`, kept here
/// so this crate needs no dependency on the chain bridge (and so no
/// Solana client) to state which program's accounts it is willing to
/// believe. The test below re-reads the deployment record, so the copy
/// cannot drift into pointing at a program that never writes proposals.
pub const GOVERNANCE_PROGRAM_ID: &str = "AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE";

/// Byte offsets into a `Proposal` account, in the program's declaration
/// order: discriminator(8), id(8), category(1), proposer(32),
/// title_hash(32), summary_hash(32), stake_deposit(8), votes_for(8),
/// votes_against(8), quorum_snapshot(8), threshold_snapshot(2),
/// created_at(8), voting_ends_at(8), state(1), quorum_met(1),
/// deposit_settled(1), executed(1), bump(1), offchain_id_hash(32).
mod offsets {
    pub const ID: usize = 8;
    pub const VOTES_FOR: usize = 8 + 8 + 1 + 32 + 32 + 32 + 8;
    pub const VOTES_AGAINST: usize = VOTES_FOR + 8;
    pub const VOTING_ENDS_AT: usize = VOTES_AGAINST + 8 + 8 + 2 + 8;
    pub const STATE: usize = VOTING_ENDS_AT + 8;
    pub const QUORUM_MET: usize = STATE + 1;
    pub const DEPOSIT_SETTLED: usize = QUORUM_MET + 1;
    pub const EXECUTED: usize = DEPOSIT_SETTLED + 1;
    pub const OFFCHAIN_ID_HASH: usize = EXECUTED + 1 + 1;
    pub const LEN: usize = OFFCHAIN_ID_HASH + 32;
}

/// The program's `ProposalState`, decoded from its Borsh discriminant.
///
/// Kept as its own type rather than mapped straight onto
/// [`ProposalStatus`], because the two enums are not the same set:
/// `Withdrawn` and `Activated` are off-chain lifecycle states the program
/// knows nothing about, and `Draft` is an on-chain state this layer has
/// no counterpart for. Collapsing them at decode time would throw away
/// the distinction between "the chain has not decided" and "the chain
/// decided nothing applies".
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OnchainProposalState {
    Draft,
    Voting,
    Accepted,
    Rejected,
}

impl OnchainProposalState {
    fn from_discriminant(raw: u8) -> Result<Self, GovernanceError> {
        match raw {
            0 => Ok(Self::Draft),
            1 => Ok(Self::Voting),
            2 => Ok(Self::Accepted),
            3 => Ok(Self::Rejected),
            _ => Err(GovernanceError::MalformedProposal),
        }
    }

    /// The off-chain status this on-chain state resolves to, if it
    /// resolves to one at all.
    ///
    /// `Draft` and `Voting` return `None` — not because they are
    /// unrepresentable, but because they are not *resolutions*. Adopting
    /// them would overwrite a local status with a non-answer.
    pub fn resolution(self) -> Option<ProposalStatus> {
        match self {
            Self::Accepted => Some(ProposalStatus::Accepted),
            Self::Rejected => Some(ProposalStatus::Rejected),
            Self::Draft | Self::Voting => None,
        }
    }
}

/// The fields of an on-chain `Proposal` that a node reading the chain
/// actually needs. Everything else on the account — the proposer, the
/// content hashes, the deposit bookkeeping — is either already known
/// off-chain or irrelevant to whether the two records agree, and is
/// deliberately left undecoded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OnchainProposal {
    pub id: u64,
    pub state: OnchainProposalState,
    /// The chain's stake-weighted totals, carried so a client can see
    /// *why* the chain decided what it did rather than only what.
    pub votes_for: u64,
    pub votes_against: u64,
    pub quorum_met: bool,
    pub deposit_settled: bool,
    pub executed: bool,
    pub voting_ends_at: i64,
    /// The off-chain proposal this account claims, or `None` when the
    /// all-zero sentinel says none was ever claimed.
    #[serde(with = "optional_hash")]
    pub offchain_id_hash: Option<[u8; 32]>,
}

/// The on-chain half of the join key for an off-chain proposal id.
///
/// SHA-256 of the id's UTF-8 bytes, matching what a client passes to the
/// program's `link_offchain_proposal`. Defined here, in the crate that
/// owns [`ProposalId`], so there is exactly one answer to "which hash,
/// over which bytes" — the ambiguity that made `title_hash` unusable as a
/// join key in the first place.
pub fn offchain_id_hash(id: &ProposalId) -> [u8; 32] {
    openfiat_crypto::sha256(id.as_str().as_bytes())
}

/// PDA seed for a `Proposal`, matching the program's own
/// `PROPOSAL_SEED`.
const PROPOSAL_SEED: &[u8] = b"proposal";

/// The address of the on-chain `Proposal` account with this `u64` id.
///
/// `[b"proposal", id.to_le_bytes()]` under the governance program, which
/// is what `create_proposal` derives. Returned by the node's
/// `getProposalChainLink` so a client can fetch the chain's own record —
/// or paste it into an explorer — without re-deriving an address that has
/// to agree with the program byte for byte.
pub fn onchain_proposal_address(id: u64) -> String {
    let program = solana_pubkey::Pubkey::from_str_const(GOVERNANCE_PROGRAM_ID);
    solana_pubkey::Pubkey::find_program_address(&[PROPOSAL_SEED, &id.to_le_bytes()], &program)
        .0
        .to_string()
}

/// Decodes a `Proposal` account's raw bytes.
///
/// `owner` is the account's owning program, as reported by the RPC. It is
/// checked rather than assumed: without that, a node would read an
/// account somebody else wrote at an address they chose and believe
/// whatever governance outcome it contained.
pub fn decode_proposal(owner: &str, data: &[u8]) -> Result<OnchainProposal, GovernanceError> {
    if owner != GOVERNANCE_PROGRAM_ID {
        return Err(GovernanceError::Unauthorized);
    }
    if data.len() < offsets::LEN || data[..8] != PROPOSAL_DISCRIMINATOR {
        return Err(GovernanceError::MalformedProposal);
    }

    let u64_at = |offset: usize| {
        u64::from_le_bytes(
            data[offset..offset + 8]
                .try_into()
                .expect("slice is exactly 8 bytes"),
        )
    };
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&data[offsets::OFFCHAIN_ID_HASH..offsets::OFFCHAIN_ID_HASH + 32]);

    Ok(OnchainProposal {
        id: u64_at(offsets::ID),
        state: OnchainProposalState::from_discriminant(data[offsets::STATE])?,
        votes_for: u64_at(offsets::VOTES_FOR),
        votes_against: u64_at(offsets::VOTES_AGAINST),
        quorum_met: data[offsets::QUORUM_MET] != 0,
        deposit_settled: data[offsets::DEPOSIT_SETTLED] != 0,
        executed: data[offsets::EXECUTED] != 0,
        voting_ends_at: u64_at(offsets::VOTING_ENDS_AT) as i64,
        // All zeroes is the program's "nothing claimed" sentinel, and
        // the program refuses to write it as a link, so the two cases
        // cannot be confused.
        offchain_id_hash: (hash != [0u8; 32]).then_some(hash),
    })
}

/// Whether the two records are the same proposal, and whether they agree.
///
/// Every variant is a distinct thing a client may need to say out loud.
/// Collapsing any of them into "no" is what produced the original defect:
/// an interface that cannot distinguish "the chain has not decided yet"
/// from "the chain decided the opposite" will show one and mean the
/// other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainAgreement {
    /// The off-chain proposal claims no on-chain counterpart. Not an
    /// error: informational proposals legitimately never go on chain.
    NoClaim,
    /// One side names the other and the other does not name it back —
    /// including the case where the on-chain proposal was never linked
    /// at all. The records are *not* joined, and nothing about the
    /// chain's tally may be attributed to this off-chain proposal.
    ClaimNotReciprocated,
    /// Both sides name each other, and the chain has not resolved yet.
    /// The honest answer while voting is open.
    LinkedStillVoting,
    /// Both sides name each other, the chain has resolved, and this node
    /// has not adopted the answer yet.
    ///
    /// Distinct from [`Self::LinkedDisagreed`] because every linked
    /// proposal passes through it in the window between the chain
    /// deciding and the next poll tick: local state says `Voting`, which
    /// is not a contradiction of anything, it is an absence of an answer.
    /// Calling that a divergence would make divergence meaningless.
    LinkedAwaitingAdoption,
    /// Both sides name each other and their outcomes match.
    LinkedAgreed,
    /// Both sides name each other and their outcomes differ. The chain's
    /// answer is the authoritative one; this exists so a client can say
    /// that a divergence happened rather than quietly overwrite it.
    LinkedDisagreed,
}

/// An off-chain proposal beside the chain's record of it, with the
/// verdict on whether they are the same proposal and whether they agree.
///
/// Both records are carried whole rather than merged into one flattened
/// view: merging is exactly the operation that loses the disagreement.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProposalChainView {
    pub offchain: Proposal,
    /// `None` when this node has no on-chain record — either the
    /// off-chain proposal claims none, or the account has not been read.
    pub onchain: Option<OnchainProposal>,
    pub agreement: ChainAgreement,
}

/// Compares an off-chain proposal with an on-chain account that may or
/// may not be its counterpart.
///
/// `onchain` of `None` means this node holds no chain record — which is
/// [`ChainAgreement::NoClaim`] if none was ever claimed, and
/// [`ChainAgreement::ClaimNotReciprocated`] if one was, because a claim
/// this node cannot corroborate is precisely an unreciprocated one.
pub fn compare(offchain: &Proposal, onchain: Option<&OnchainProposal>) -> ChainAgreement {
    let Some(claimed_id) = offchain.onchain_proposal_id else {
        return ChainAgreement::NoClaim;
    };
    let Some(onchain) = onchain else {
        return ChainAgreement::ClaimNotReciprocated;
    };
    // Both directions, every time. `id` matching is not enough on its
    // own: it only proves this node fetched the account the off-chain
    // record pointed at, which the off-chain record's author chose.
    if onchain.id != claimed_id || onchain.offchain_id_hash != Some(offchain_id_hash(&offchain.id))
    {
        return ChainAgreement::ClaimNotReciprocated;
    }

    match onchain.state.resolution() {
        None => ChainAgreement::LinkedStillVoting,
        Some(resolved) if resolved == offchain.status => ChainAgreement::LinkedAgreed,
        Some(_) if offchain.status == ProposalStatus::Voting => {
            ChainAgreement::LinkedAwaitingAdoption
        }
        Some(_) => ChainAgreement::LinkedDisagreed,
    }
}

/// `Option<[u8; 32]>` as an optional lowercase hex string, so a JSON-RPC
/// client sees the same digest it would paste into an explorer rather
/// than a 32-element array of numbers.
mod optional_hash {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<[u8; 32]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(bytes) => {
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                serializer.serialize_some(&hex)
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<[u8; 32]>, D::Error> {
        let Some(hex) = Option::<String>::deserialize(deserializer)? else {
            return Ok(None);
        };
        if hex.len() != 64 {
            return Err(serde::de::Error::custom(
                "expected a 64-character hex digest",
            ));
        }
        let mut bytes = [0u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(Some(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::ProposalCategory;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_types::Timestamp;

    /// A well-formed `Proposal` account's raw bytes.
    fn proposal_bytes(id: u64, state: u8, quorum_met: bool, offchain_id_hash: [u8; 32]) -> Vec<u8> {
        let mut bytes = vec![0u8; offsets::LEN];
        bytes[..8].copy_from_slice(&PROPOSAL_DISCRIMINATOR);
        bytes[offsets::ID..offsets::ID + 8].copy_from_slice(&id.to_le_bytes());
        bytes[offsets::VOTES_FOR..offsets::VOTES_FOR + 8].copy_from_slice(&900u64.to_le_bytes());
        bytes[offsets::VOTES_AGAINST..offsets::VOTES_AGAINST + 8]
            .copy_from_slice(&100u64.to_le_bytes());
        bytes[offsets::VOTING_ENDS_AT..offsets::VOTING_ENDS_AT + 8]
            .copy_from_slice(&1_700_000_000u64.to_le_bytes());
        bytes[offsets::STATE] = state;
        bytes[offsets::QUORUM_MET] = quorum_met as u8;
        bytes[offsets::OFFCHAIN_ID_HASH..offsets::OFFCHAIN_ID_HASH + 32]
            .copy_from_slice(&offchain_id_hash);
        bytes
    }

    fn offchain_proposal(id: &str, claims: Option<u64>, status: ProposalStatus) -> Proposal {
        let author = Keypair::generate();
        Proposal {
            id: ProposalId::new(id),
            title: "Raise the reservation timeout".to_string(),
            summary: "From 30 to 45 minutes.".to_string(),
            category: ProposalCategory::Protocol,
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            status,
            votes: Vec::new(),
            onchain_proposal_id: claims,
            voting_closes_at: Timestamp::from_millis(2_000),
            created_at: Timestamp::from_millis(1_000),
            updated_at: Timestamp::from_millis(1_000),
        }
    }

    /// The layout this module hard-codes, checked against the IDL a real
    /// `anchor build` produced. Without this, a field added to the
    /// program's `Proposal` would shift every offset here and this
    /// decoder would keep returning confident nonsense — reading, say,
    /// `deposit_settled` as the accept/reject outcome.
    ///
    /// Read at run time rather than with `include_str!`, because
    /// `programs/target` is generated and git-ignored: an `include_str!`
    /// would make this crate impossible to compile from a fresh clone
    /// until somebody ran `anchor build`. Absent the file the check
    /// cannot be made, and it says so instead of passing quietly — a
    /// test that reports success when it could not look is worse than no
    /// test. It runs for every developer who has built the programs, and
    /// in the `programs-ci` job that builds them.
    #[test]
    fn the_hard_coded_layout_still_matches_the_programs_own_idl() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../programs/target/idl/governance.json"
        );
        let Ok(raw) = std::fs::read_to_string(path) else {
            eprintln!(
                "SKIPPED: {path} is absent, so this crate's hard-coded Proposal layout could \
                 not be checked against the program's own IDL. Run `anchor build` in \
                 openfiat-core/programs to enable it."
            );
            return;
        };
        let idl: serde_json::Value =
            serde_json::from_str(&raw).expect("the IDL must be valid JSON");

        let discriminator: Vec<u8> = idl["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["name"] == "Proposal")
            .expect("the program must still declare a Proposal account")["discriminator"]
            .as_array()
            .unwrap()
            .iter()
            .map(|byte| byte.as_u64().unwrap() as u8)
            .collect();
        assert_eq!(discriminator, PROPOSAL_DISCRIMINATOR);

        let fields = idl["types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ty| ty["name"] == "Proposal")
            .expect("the IDL must describe Proposal's fields")["type"]["fields"]
            .as_array()
            .unwrap()
            .clone();

        // Widths are recomputed from the IDL's own type names rather than
        // assumed, so a field whose *type* changed is caught as well as
        // one that was inserted.
        fn width(ty: &serde_json::Value) -> usize {
            if let Some(name) = ty.as_str() {
                return match name {
                    "u8" | "bool" => 1,
                    "u16" => 2,
                    "i64" | "u64" => 8,
                    "pubkey" => 32,
                    other => panic!("unhandled IDL scalar type {other}"),
                };
            }
            if let Some(array) = ty.get("array") {
                let element = width(&array[0]);
                return element * array[1].as_u64().unwrap() as usize;
            }
            // Both `defined` types on `Proposal` — ProposalCategory and
            // ProposalState — are fieldless enums, one Borsh byte each.
            assert!(ty.get("defined").is_some(), "unhandled IDL type {ty}");
            1
        }

        let mut offset = 8; // the discriminator
        let mut actual = std::collections::HashMap::new();
        for field in &fields {
            actual.insert(field["name"].as_str().unwrap().to_string(), offset);
            offset += width(&field["type"]);
        }

        assert_eq!(actual.get("id"), Some(&offsets::ID));
        assert_eq!(actual.get("votes_for"), Some(&offsets::VOTES_FOR));
        assert_eq!(actual.get("votes_against"), Some(&offsets::VOTES_AGAINST));
        assert_eq!(actual.get("voting_ends_at"), Some(&offsets::VOTING_ENDS_AT));
        assert_eq!(actual.get("state"), Some(&offsets::STATE));
        assert_eq!(actual.get("quorum_met"), Some(&offsets::QUORUM_MET));
        assert_eq!(
            actual.get("deposit_settled"),
            Some(&offsets::DEPOSIT_SETTLED)
        );
        assert_eq!(actual.get("executed"), Some(&offsets::EXECUTED));
        assert_eq!(
            actual.get("offchain_id_hash"),
            Some(&offsets::OFFCHAIN_ID_HASH),
            "the program must still carry the off-chain join key"
        );
        assert_eq!(offset, offsets::LEN);
    }

    /// The program id this crate is willing to believe, checked against
    /// the record of what was actually deployed. A typo would leave every
    /// chain read rejected as "not owned by governance", which looks
    /// exactly like a chain that has no proposals.
    /// `devnet-addresses.json` *is* tracked, unlike the generated IDL
    /// above, so this one can be a compile-time `include_str!` and a
    /// hard failure.
    #[test]
    fn the_pinned_program_id_matches_the_deployment_record() {
        const ADDRESSES: &str = include_str!("../../../programs/devnet-addresses.json");
        let addresses: serde_json::Value =
            serde_json::from_str(ADDRESSES).expect("devnet-addresses.json must be valid");
        assert_eq!(
            addresses["devnet_programs"]["governance"].as_str(),
            Some(GOVERNANCE_PROGRAM_ID)
        );
    }

    #[test]
    fn decodes_the_fields_a_resolution_depends_on() {
        let hash = offchain_id_hash(&ProposalId::new("ofip-0001"));
        let decoded =
            decode_proposal(GOVERNANCE_PROGRAM_ID, &proposal_bytes(7, 2, true, hash)).unwrap();
        assert_eq!(decoded.id, 7);
        assert_eq!(decoded.state, OnchainProposalState::Accepted);
        assert_eq!(decoded.state.resolution(), Some(ProposalStatus::Accepted));
        assert!(decoded.quorum_met);
        assert_eq!(decoded.votes_for, 900);
        assert_eq!(decoded.votes_against, 100);
        assert_eq!(decoded.offchain_id_hash, Some(hash));
    }

    #[test]
    fn an_account_owned_by_another_program_is_refused() {
        // The whole soundness of reading a governance outcome off the
        // chain rests on this: anyone can create an account at an address
        // of their choosing and fill it with a passing tally.
        let hash = offchain_id_hash(&ProposalId::new("ofip-0001"));
        let result = decode_proposal(
            "HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP",
            &proposal_bytes(7, 2, true, hash),
        );
        assert_eq!(result, Err(GovernanceError::Unauthorized));
    }

    #[test]
    fn an_account_with_another_types_discriminator_is_refused() {
        let mut bytes = proposal_bytes(7, 2, true, [9u8; 32]);
        bytes[0] ^= 0xFF;
        assert_eq!(
            decode_proposal(GOVERNANCE_PROGRAM_ID, &bytes),
            Err(GovernanceError::MalformedProposal)
        );
    }

    #[test]
    fn an_unlinked_onchain_proposal_reports_no_hash_rather_than_zeroes() {
        let decoded = decode_proposal(
            GOVERNANCE_PROGRAM_ID,
            &proposal_bytes(7, 1, false, [0u8; 32]),
        )
        .unwrap();
        assert_eq!(decoded.offchain_id_hash, None);
    }

    #[test]
    fn a_proposal_claiming_nothing_is_not_treated_as_linked() {
        let offchain = offchain_proposal("ofip-0001", None, ProposalStatus::Voting);
        assert_eq!(compare(&offchain, None), ChainAgreement::NoClaim);
    }

    #[test]
    fn a_one_sided_claim_is_reported_as_one_sided_rather_than_joined() {
        // The property the whole two-claim design exists for. The
        // off-chain record points at on-chain proposal 7, and on-chain
        // proposal 7 has never heard of it — so the chain's `Accepted`
        // says nothing about this proposal and must not be adopted.
        let offchain = offchain_proposal("ofip-0001", Some(7), ProposalStatus::Voting);
        let onchain = decode_proposal(
            GOVERNANCE_PROGRAM_ID,
            &proposal_bytes(7, 2, true, [0u8; 32]),
        )
        .unwrap();
        assert_eq!(
            compare(&offchain, Some(&onchain)),
            ChainAgreement::ClaimNotReciprocated
        );
    }

    #[test]
    fn a_claim_answered_by_a_different_offchain_proposal_is_not_a_link() {
        let offchain = offchain_proposal("ofip-0001", Some(7), ProposalStatus::Voting);
        let other = offchain_id_hash(&ProposalId::new("ofip-0002"));
        let onchain =
            decode_proposal(GOVERNANCE_PROGRAM_ID, &proposal_bytes(7, 2, true, other)).unwrap();
        assert_eq!(
            compare(&offchain, Some(&onchain)),
            ChainAgreement::ClaimNotReciprocated
        );
    }

    #[test]
    fn a_reciprocated_claim_whose_outcomes_match_is_agreement() {
        let offchain = offchain_proposal("ofip-0001", Some(7), ProposalStatus::Accepted);
        let hash = offchain_id_hash(&offchain.id);
        let onchain =
            decode_proposal(GOVERNANCE_PROGRAM_ID, &proposal_bytes(7, 2, true, hash)).unwrap();
        assert_eq!(
            compare(&offchain, Some(&onchain)),
            ChainAgreement::LinkedAgreed
        );
    }

    #[test]
    fn a_linked_proposal_the_chain_has_not_decided_is_not_called_agreement() {
        let offchain = offchain_proposal("ofip-0001", Some(7), ProposalStatus::Voting);
        let hash = offchain_id_hash(&offchain.id);
        let onchain =
            decode_proposal(GOVERNANCE_PROGRAM_ID, &proposal_bytes(7, 1, false, hash)).unwrap();
        assert_eq!(
            compare(&offchain, Some(&onchain)),
            ChainAgreement::LinkedStillVoting
        );
    }

    #[test]
    fn the_lag_between_the_chain_deciding_and_this_node_adopting_is_not_a_disagreement() {
        // The chain has accepted; this node has not adopted it yet. Every
        // linked proposal passes through this state, so reporting it as a
        // divergence would make divergence meaningless.
        let offchain = offchain_proposal("ofip-0001", Some(7), ProposalStatus::Voting);
        let hash = offchain_id_hash(&offchain.id);
        let onchain =
            decode_proposal(GOVERNANCE_PROGRAM_ID, &proposal_bytes(7, 2, true, hash)).unwrap();
        assert_eq!(
            compare(&offchain, Some(&onchain)),
            ChainAgreement::LinkedAwaitingAdoption
        );
    }

    #[test]
    fn a_genuine_divergence_is_reported_rather_than_hidden() {
        // Locally activated (which only follows a local `Accepted`),
        // while the chain rejected. Precisely the case an interface
        // showing one record could never surface.
        let offchain = offchain_proposal("ofip-0001", Some(7), ProposalStatus::Activated);
        let hash = offchain_id_hash(&offchain.id);
        let onchain =
            decode_proposal(GOVERNANCE_PROGRAM_ID, &proposal_bytes(7, 3, true, hash)).unwrap();
        assert_eq!(
            compare(&offchain, Some(&onchain)),
            ChainAgreement::LinkedDisagreed
        );
    }

    /// Pinned against addresses derived independently by
    /// `@solana/web3.js`'s `findProgramAddressSync`, the same routine the
    /// TypeScript SDK and the program's own test suite use. An address
    /// this crate derived differently would send every client to an empty
    /// account, which looks exactly like a proposal that was never
    /// created.
    #[test]
    fn a_proposal_address_matches_what_a_solana_client_derives() {
        for (id, expected) in [
            (0u64, "8TACBS35SgEU3RDQSqs8hmCPCRGAY5LNVfvBLHoJrYS8"),
            (1, "G7EgazwBMorPCPcHyY9ySSL6dBVithAuEg5L1K2Gj38d"),
            (7, "CLTLFb1YRbg3seL6xQMYM6fLQiMUHyjoC6orQ7UMgj7G"),
            (70_000, "AmBots1BbXKRnTHnqBEkNnDxa8VAoJCgHu1ccy8MdAhd"),
        ] {
            assert_eq!(onchain_proposal_address(id), expected);
        }
    }

    #[test]
    fn the_join_key_is_the_sha256_of_the_id_and_nothing_else() {
        // Pinned against a literal digest rather than against
        // `sha256(...)` recomputed the same way, so a change of hash
        // function or of the bytes hashed fails here instead of agreeing
        // with itself. A client that hashed differently would produce a
        // link the program stores and this crate never matches.
        let hex: String = offchain_id_hash(&ProposalId::new("ofip-0001"))
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            hex,
            "4298e38755cdf4009f0a9beb84960a10c0705ca506826fc77eb8cfd1a2b40ef1"
        );
    }
}
