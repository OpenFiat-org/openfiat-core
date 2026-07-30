use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};

use crate::{constants::*, error::ErrorCode, state::*};

/// Corrects the singleton `FeeConfig` after `initialize_fee_config` has
/// run — admin-only, matching `FeeConfig`'s own "governance-updatable in
/// a later phase, for now updatable only by `admin`" note.
///
/// The four treasuries arrive as **typed token accounts constrained to a
/// mint**, not as plain `Pubkey` params like `initialize_fee_config`
/// takes them. That difference is the point of this instruction existing
/// at all: the deployed config was initialized with the treasury *owner*
/// wallets rather than their token accounts, and since `release_escrow`
/// requires each treasury to deserialize as a `TokenAccount`, the whole
/// release path — every settlement and every `BuyerWins` dispute — could
/// not execute. Nothing rejected the bad values at write time, because
/// nothing checked them.
///
/// Taking them as accounts means the runtime does the checking: a wallet
/// address cannot be passed where a `TokenAccount` is required, and
/// `token::mint = mint` forces all four to share one mint, so a config
/// that would fail at release time cannot be stored in the first place.
#[derive(Accounts)]
pub struct UpdateFeeConfig<'info> {
    pub admin: Signer<'info>,

    #[account(
        mut,
        seeds = [FEE_CONFIG_SEED],
        bump = fee_config.bump,
        constraint = fee_config.admin == admin.key() @ ErrorCode::Unauthorized,
    )]
    pub fee_config: Box<Account<'info, FeeConfig>>,

    /// The mint every treasury must hold. Not stored on `FeeConfig`, so
    /// this enforces the four are mutually consistent rather than
    /// checking them against a recorded mint — see this file's own note
    /// on the remaining gap.
    pub mint: Box<InterfaceAccount<'info, Mint>>,

    #[account(token::mint = mint)]
    pub dev_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(token::mint = mint)]
    pub ecosystem_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(token::mint = mint)]
    pub infra_treasury: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(token::mint = mint)]
    pub emergency_reserve: Box<InterfaceAccount<'info, TokenAccount>>,
}

/// The numeric half of the config. The treasury addresses are absent
/// deliberately — they come from the account context above so they
/// cannot be mistyped.
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct UpdateFeeConfigParams {
    pub ad_listing_fee: u64,
    pub dispute_filing_fee: u64,
    pub settlement_fee_bps: u16,
    pub dev_treasury_bps: u16,
    pub ecosystem_treasury_bps: u16,
    pub infra_treasury_bps: u16,
    pub emergency_reserve_bps: u16,
    pub timeout_secs: i64,
    /// Arbitrator stake age in seconds; zero disables the gate. This is
    /// the path OFS-4100 §4's 30 days is actually turned on through — see
    /// `RECOMMENDED_MIN_ARBITRATOR_STAKE_AGE_SECS` for why it cannot be
    /// enabled at deployment.
    pub min_arbitrator_stake_age_secs: i64,
    /// Opening sortition threshold in basis points; zero disables the
    /// draw. Values at or above `BPS_DENOMINATOR` would admit every
    /// wallet, which is the same as disabled but harder to read, so they
    /// are rejected outright rather than silently accepted.
    pub arbitrator_sortition_bps: u16,
    /// The complete settlement-mint allowlist, replacing whatever is
    /// stored. This is the governance path the steward's "governance can
    /// vote to add more tokens" is exercised through, and — because it is
    /// a replacement rather than an append — the only way to de-list.
    ///
    /// A replacement rather than an add/remove pair for a reason that
    /// matters more than convenience: the caller states the full intended
    /// list, so what is stored afterwards is exactly what was reviewed and
    /// voted on. An `add_mint` instruction makes the stored list the
    /// accumulated result of every call ever made, which is not a thing
    /// anybody voted for.
    ///
    /// De-listing strands nothing. Existing vaults keep their balances and
    /// stay withdrawable; only new reservations and new escrows are
    /// refused. See `reserve_liquidity` and `create_trade_escrow`, which
    /// are the two places the check lives, and `withdraw_liquidity`, which
    /// deliberately has no check at all.
    pub settlement_mints: Vec<Pubkey>,
}

pub fn handle_update_fee_config(
    ctx: Context<UpdateFeeConfig>,
    params: UpdateFeeConfigParams,
) -> Result<()> {
    require!(
        params.settlement_fee_bps as u64 <= BPS_DENOMINATOR,
        ErrorCode::InvalidFeeBps
    );
    let split_total = params.dev_treasury_bps as u64
        + params.ecosystem_treasury_bps as u64
        + params.infra_treasury_bps as u64
        + params.emergency_reserve_bps as u64;
    require!(split_total == BPS_DENOMINATOR, ErrorCode::InvalidFeeSplit);
    require!(params.timeout_secs > 0, ErrorCode::InvalidTimeout);
    // A negative age would make every comparison against it vacuously
    // true, which reads in the account as "a requirement is set" while
    // enforcing nothing — worse than an honest zero.
    require!(
        params.min_arbitrator_stake_age_secs >= 0,
        ErrorCode::InvalidStakeAge
    );
    require!(
        (params.arbitrator_sortition_bps as u64) < BPS_DENOMINATOR,
        ErrorCode::InvalidSortitionThreshold
    );

    // An empty list refuses every trade. That is indistinguishable from
    // pausing the protocol, and pausing the protocol is not a fee
    // parameter — if it is ever wanted it should be its own instruction
    // with its own name, so it cannot happen as a side effect of somebody
    // sending an under-populated params struct.
    require!(
        !params.settlement_mints.is_empty(),
        ErrorCode::EmptySettlementMintList
    );
    require!(
        params.settlement_mints.len() <= MAX_SETTLEMENT_MINTS,
        ErrorCode::SettlementMintListFull
    );
    for (i, mint) in params.settlement_mints.iter().enumerate() {
        // `Pubkey::default()` is the array's own padding value, so storing
        // it inside the live prefix would make a mint that was never
        // configured indistinguishable from one that was — and an omitted
        // account argument deserializes to exactly this key.
        require_keys_neq!(*mint, Pubkey::default(), ErrorCode::InvalidSettlementMint);
        // A duplicate is harmless to the lookup but silently wastes a slot
        // and misreports the list's real size to anyone reading the
        // account, so it is refused rather than deduplicated: a caller who
        // sent a list they did not mean should find out.
        require!(
            !params.settlement_mints[..i].contains(mint),
            ErrorCode::InvalidSettlementMint
        );
    }

    let fee_config = &mut ctx.accounts.fee_config;
    fee_config.ad_listing_fee = params.ad_listing_fee;
    fee_config.dispute_filing_fee = params.dispute_filing_fee;
    fee_config.settlement_fee_bps = params.settlement_fee_bps;
    fee_config.dev_treasury = ctx.accounts.dev_treasury.key();
    fee_config.ecosystem_treasury = ctx.accounts.ecosystem_treasury.key();
    fee_config.infra_treasury = ctx.accounts.infra_treasury.key();
    fee_config.emergency_reserve = ctx.accounts.emergency_reserve.key();
    fee_config.dev_treasury_bps = params.dev_treasury_bps;
    fee_config.ecosystem_treasury_bps = params.ecosystem_treasury_bps;
    fee_config.infra_treasury_bps = params.infra_treasury_bps;
    fee_config.emergency_reserve_bps = params.emergency_reserve_bps;
    fee_config.timeout_secs = params.timeout_secs;
    fee_config.min_arbitrator_stake_age_secs = params.min_arbitrator_stake_age_secs;
    fee_config.arbitrator_sortition_bps = params.arbitrator_sortition_bps;
    // Cleared in full before the new list is written. Leaving the old tail
    // in place would be invisible while `settlement_mint_count` is honoured
    // — `allows_settlement_mint` never reads past the count — but it would
    // leave de-listed mints sitting in the account for anyone decoding the
    // raw bytes, which is exactly the sort of "is this still allowed?"
    // ambiguity an allowlist must not have.
    fee_config.settlement_mints = [Pubkey::default(); MAX_SETTLEMENT_MINTS];
    fee_config.settlement_mints[..params.settlement_mints.len()]
        .copy_from_slice(&params.settlement_mints);
    fee_config.settlement_mint_count = params.settlement_mints.len() as u8;
    // `admin` is intentionally not updatable here — handing over control
    // is a distinct action from correcting fee parameters, and folding it
    // into this instruction would let one fat-fingered call lock the
    // config permanently.
    Ok(())
}
