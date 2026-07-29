use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, events::WalletDelisted, state::*};

/// Removes a wallet from the ban list (OFS-7100 §12.2), restoring its
/// deposit access protocol-wide.
///
/// Closing the `BanRecord` returns its address to the "non-existent
/// account" state every gate treats as unbanned, so access is restored
/// everywhere by the same single account operation that removed it — no
/// per-program cleanup, and no window in which some programs still
/// refuse the wallet.
///
/// §12.2 makes this mandatory rather than optional, and §15 explains the
/// cost of getting it wrong: once rejection is protocol-wide, "an
/// erroneous listing now costs a wallet all protocol access rather than
/// one application's caution". A ban list without a working delist path
/// would turn every false positive into a permanent one.
///
/// Authority is `GovernanceConfig.admin`, identical to `list_wallet` —
/// see that instruction's doc comment for the honest account of what
/// that authority is and what it would take to make it genuinely
/// governance-executed. The two must stay in step: an exclusion power
/// wider than the readmission power is the failure §12.2 names.
///
/// Rent goes back to `admin`, who paid it at listing. This is a refund,
/// not a reward — the banned wallet never funded the record, so nothing
/// here is taken from it. That asymmetry is deliberate: a design that
/// made the listed wallet pay would let anyone impose a cost on any
/// wallet, and a design that burned the rent would make delisting
/// quietly expensive for the party we most want to keep willing to
/// reverse mistakes.
#[derive(Accounts)]
#[instruction(wallet: Pubkey)]
pub struct DelistWallet<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        seeds = [GOVERNANCE_CONFIG_SEED],
        bump = governance_config.bump,
        constraint = governance_config.admin == admin.key() @ ErrorCode::Unauthorized,
    )]
    pub governance_config: Box<Account<'info, GovernanceConfig>>,

    /// Seeded on the `wallet` argument, so the address is derived rather
    /// than trusted; `close` then hands the rent back to `admin` and
    /// zeroes the account, which is what the gates read as unbanned.
    #[account(
        mut,
        close = admin,
        seeds = [BAN_SEED, wallet.as_ref()],
        bump = ban_record.bump,
    )]
    pub ban_record: Account<'info, BanRecord>,
}

pub fn handle_delist_wallet(ctx: Context<DelistWallet>, wallet: Pubkey) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    emit!(WalletDelisted {
        wallet,
        delisted_by: ctx.accounts.admin.key(),
        listed_at: ctx.accounts.ban_record.listed_at,
        timestamp: now,
    });
    Ok(())
}
