use anchor_lang::prelude::*;

/// PDA seed for the singleton `GovernanceConfig` account (OFS-4200 §6).
#[constant]
pub const GOVERNANCE_CONFIG_SEED: &[u8] = b"governance_config";

/// PDA seed for the token vault holding proposal-stake deposits until
/// `refund_or_forfeit_deposit` runs. Not named explicitly in OFS-4200
/// §6 — this workspace's own extension, matching every other program's
/// "singleton config owns a singleton vault" pattern.
#[constant]
pub const DEPOSIT_VAULT_SEED: &[u8] = b"deposit_vault";

/// PDA seed for a `Proposal`: `[SEED, id.to_le_bytes()]` (OFS-4200 §6).
#[constant]
pub const PROPOSAL_SEED: &[u8] = b"proposal";

/// PDA seed for a `VoteRecord`: `[SEED, proposal, voter]` (OFS-4200 §6)
/// — its existence is itself the double-vote guard.
#[constant]
pub const VOTE_RECORD_SEED: &[u8] = b"vote";

/// Basis-points denominator (10_000 = 100%), matching every other
/// program in this workspace.
#[constant]
pub const BPS_DENOMINATOR: u64 = 10_000;
