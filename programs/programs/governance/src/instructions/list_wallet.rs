use anchor_lang::prelude::*;

use crate::{constants::*, error::ErrorCode, events::WalletListed, state::*};

/// Adds a wallet to the protocol-wide ban list (OFS-7100 §12).
///
/// Creating the `BanRecord` PDA *is* the ban. Every gated instruction in
/// `escrow`, `staking`, `presale` and this program derives the same
/// address from its own signer's key and refuses to run if it is
/// occupied, so this one account creation closes deposit access
/// protocol-wide in a single transaction. Nothing else has to be
/// notified and no application can opt out — which is precisely what
/// §12 asks for, and precisely why the authority question below matters.
///
/// # Authority: admin-gated, NOT governance-executed  `[PROPOSED — NEEDS SIGN-OFF]`
///
/// §12.2 says "Only governance may add or remove entries". This
/// instruction does **not** implement that. It checks
/// `GovernanceConfig.admin` — a single key — and nothing about this
/// instruction consults a proposal, a vote, or a quorum.
///
/// The gap is not an oversight in this instruction, it is a missing
/// capability in the program. `update_config_parameter` and
/// `authorize_treasury_spend` are the two instructions a passed proposal
/// is supposed to act through, and both only set `Proposal::executed =
/// true` and return; they record an authorization, they do not perform
/// one. There is therefore no working path today by which a tallied,
/// accepted proposal can cause any state change at all — here or in any
/// other program. Gating this on a proposal would mean gating it on
/// machinery that cannot fire, i.e. a ban list that can never be used.
///
/// So the honest description of the power this creates is: **one key,
/// `GovernanceConfig.admin`, can deny any wallet deposit access to the
/// entire protocol, and the same key can restore it.** It should not be
/// described as governance-controlled in any interface, doc, or address
/// record until the paragraph below is closed out. `devnet-addresses.json`
/// records the same caveat next to the deployed program.
///
/// What closing it requires, concretely: an execution path that lets a
/// finalized `Proposal` authorize a state change (a proposal-signed PDA
/// authority, or a CPI from a `execute_proposal` instruction that
/// verifies `state == Accepted && quorum_met && !executed`), and then
/// re-gating this instruction and `delist_wallet` on that instead of on
/// `admin`. Delisting deliberately keeps the *same* authority as
/// listing: an authority that can exclude but not readmit would violate
/// §12.2's reversibility requirement, and §15's false-positive handling
/// depends on the readmission path being at least as available as the
/// exclusion one.
#[derive(Accounts)]
#[instruction(wallet: Pubkey)]
pub struct ListWallet<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,

    #[account(
        seeds = [GOVERNANCE_CONFIG_SEED],
        bump = governance_config.bump,
        constraint = governance_config.admin == admin.key() @ ErrorCode::Unauthorized,
    )]
    pub governance_config: Box<Account<'info, GovernanceConfig>>,

    /// `init` rather than `init_if_needed`: re-listing an
    /// already-listed wallet is a mistake worth surfacing, and letting
    /// it succeed would silently overwrite the original `listed_at` and
    /// evidence hash — the two fields an erroneously-listed wallet would
    /// use to contest the listing under §15.
    #[account(
        init,
        payer = admin,
        space = 8 + BanRecord::INIT_SPACE,
        seeds = [BAN_SEED, wallet.as_ref()],
        bump,
    )]
    pub ban_record: Account<'info, BanRecord>,

    pub system_program: Program<'info, System>,
}

pub fn handle_list_wallet(
    ctx: Context<ListWallet>,
    wallet: Pubkey,
    reason: BanReason,
    evidence_hash: [u8; 32],
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let admin = ctx.accounts.admin.key();

    let ban_record = &mut ctx.accounts.ban_record;
    ban_record.wallet = wallet;
    ban_record.reason = reason;
    ban_record.evidence_hash = evidence_hash;
    ban_record.listed_at = now;
    ban_record.listed_by = admin;
    ban_record.bump = ctx.bumps.ban_record;

    emit!(WalletListed {
        wallet,
        reason,
        evidence_hash,
        listed_by: admin,
        timestamp: now,
    });
    Ok(())
}
