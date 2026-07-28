use anchor_lang::prelude::*;

/// PDA seed for the singleton `StakingConfig` account (OFS-4200 §5).
#[constant]
pub const STAKING_CONFIG_SEED: &[u8] = b"staking_config";

/// PDA seed for the single global token vault holding every role's staked
/// OPEN (unbonding included, until withdrawn). Not named explicitly in
/// OFS-4200 §5 (which specifies `StakeAccount`'s own seed only) — this
/// workspace's own extension, one shared vault rather than one per
/// `StakeAccount` since staked funds are fungible and only ever move
/// under this program's own authority.
#[constant]
pub const STAKE_VAULT_SEED: &[u8] = b"stake_vault";

/// PDA seed for the reward pool token vault (plan decision #4 — the
/// RpcConnected/GossipOnly reward asymmetry's funding source). Fed from
/// the OPEN allocation bucket set aside for node/service-provider
/// incentives at genesis.
#[constant]
pub const REWARDS_VAULT_SEED: &[u8] = b"rewards_vault";

/// PDA seed for a `StakeAccount`: `[SEED, owner, role_as_u8]` (OFS-4200 §5).
#[constant]
pub const STAKE_ACCOUNT_SEED: &[u8] = b"stake";

/// Basis-points denominator (10_000 = 100%), matching every other program
/// in this workspace.
#[constant]
pub const BPS_DENOMINATOR: u64 = 10_000;
