use anchor_lang::prelude::*;
use openfiat_programs_shared::Role;

use crate::{constants::*, error::ErrorCode, state::*};

/// Records one arbitrator's sealed commitment for a dispute case.
///
/// Committing is gated on the wallet actually holding the Arbitrator
/// role's minimum stake. It previously was not — the reasoning being that
/// off-chain eligibility (`crates/disputes::join_as_arbitrator`) vetted
/// arbitrators before anything was relayed on-chain, and that a vote gets
/// weighted by real stake at reveal time anyway.
///
/// Both halves of that were wrong. Nothing forces a commit to arrive via
/// the off-chain path — this instruction is reachable directly — and
/// weighting at reveal does not help when the seats themselves are the
/// scarce resource. `initialize_stake_account` is permissionless and a
/// zero-balance account is a legal one, so seven throwaway wallets could
/// occupy all [`MAX_ARBITRATORS`] slots for the price of rent, reveal
/// zero-weight votes, and drive every outcome total to zero. That tie
/// resolved to `InvalidDispute`, which returns the escrow to the seller:
/// a merchant could keep a buyer's money after the buyer had already sent
/// fiat, risking no OPEN at all.
///
/// Requiring the stake at commit time makes filling every seat cost
/// `MAX_ARBITRATORS` × the arbitrator minimum in genuinely slashable
/// stake rather than rent.
///
/// # Three gates, and why the stake minimum alone was not enough
///
/// The stake gate above stopped seats being taken for the price of rent.
/// It did not stop them being taken at all: anyone holding
/// `MAX_ARBITRATORS` funded wallets could still occupy every seat on any
/// case, at the moment of their choosing, because seats went to whoever
/// called first. And that stake is never actually forfeited — slashing
/// fires for revealing *outside* consensus, and an attacker holding every
/// seat **is** the consensus. It is capital locked, not capital at risk.
///
/// So eligibility for a *specific* case is now three conditions, each
/// closing a hole the previous one leaves open (OFS-4100 §4, §4.1):
///
/// 1. **Effective stake at or above the Arbitrator minimum** — who is in
///    the pool at all.
/// 2. **Stake age** — an attacker who can create wallets on demand defeats
///    any per-case draw by simply making more of them until enough
///    qualify. The age requirement is what makes each wallet cost time as
///    well as capital, and it is why the draw below is worth anything.
/// 3. **The per-case draw** — removes the attacker's ability to *choose*
///    to take seats. They can still hope to.
///
/// Both 2 and 3 are governance parameters that ship disabled, because
/// neither can be satisfied by anybody on a chain younger than the
/// requirement it imposes; see `RECOMMENDED_MIN_ARBITRATOR_STAKE_AGE_SECS`.
///
/// # Every account here is boxed
///
/// Not tidiness. `Account<'info, T>` holds the deserialized `T` inline in
/// `try_accounts`' stack frame, and SBF gives that frame 4 KB total.
/// `FeeConfig` alone carries a `[Pubkey; 16]` — 512 bytes — and `DisputeCase`
/// carries six vectors' worth of headers; `StakingConfig` carries two
/// per-role arrays.
///
/// Unboxed, this struct did not overflow: the build reported nothing before
/// the boxes went in. What is *not* known is by how much, because the
/// toolchain says nothing at all until the frame is already over — there is
/// no margin readout, only silence and then a hard error. So "it fits today"
/// is the whole of the evidence, and it was obtained on a struct that this
/// change was about to grow (`DisputeCase` gained two fields). Boxing costs a
/// heap allocation and removes the question.
///
/// The question is worth removing because the failure is not a build failure.
/// `cargo build-sbf` reports an overflow as a non-fatal `Error:` line and
/// still emits a working-looking binary, which then dies at runtime with an
/// access violation attributed to whatever code happened to run — not to the
/// field that pushed the frame over. `execute_dispute_outcome` carries the
/// same note for the same reason, and `staking::recover_stake_shortfall` was
/// caught 8 bytes over. Keep new accounts here boxed, and keep these boxed.
#[derive(Accounts)]
pub struct CommitDisputeVote<'info> {
    pub arbitrator: Signer<'info>,

    #[account(
        mut,
        seeds = [DISPUTE_CASE_SEED, &dispute_case.reservation_id.to_le_bytes()],
        bump = dispute_case.bump,
    )]
    pub dispute_case: Box<Account<'info, DisputeCase>>,

    /// Read for the current Arbitrator-role minimum, so the gate follows a
    /// governance parameter change with no redeploy here.
    #[account(
        seeds = [staking::STAKING_CONFIG_SEED],
        seeds::program = staking::ID,
        bump = staking_config.bump,
    )]
    pub staking_config: Box<Account<'info, staking::StakingConfig>>,

    /// This arbitrator's own Arbitrator-role stake. Same constraint style
    /// as `reveal_dispute_vote`: the seeds pin both owner and role, so
    /// neither another wallet's stake nor a different role's can be
    /// substituted.
    #[account(
        seeds = [staking::STAKE_ACCOUNT_SEED, arbitrator.key().as_ref(), &[Role::Arbitrator as u8]],
        seeds::program = staking::ID,
        bump = arbitrator_stake.bump,
    )]
    pub arbitrator_stake: Box<Account<'info, staking::StakeAccount>>,

    /// Holds both arbitrator-eligibility parameters — read here rather than
    /// baked in as constants so governance can turn the age gate and the
    /// draw on without a redeploy of this program.
    #[account(seeds = [FEE_CONFIG_SEED], bump = fee_config.bump)]
    pub fee_config: Box<Account<'info, FeeConfig>>,
}

pub fn handle_commit_dispute_vote(
    ctx: Context<CommitDisputeVote>,
    commitment: [u8; 32],
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;

    require!(
        ctx.accounts
            .arbitrator_stake
            .effective_stake(&ctx.accounts.staking_config)
            >= ctx.accounts.staking_config.min_stake_for(Role::Arbitrator),
        ErrorCode::ArbitratorStakeBelowMinimum
    );

    // Checked against the stake account's own clock, which starts when its
    // balance first goes positive and clears on a full exit — so an
    // arbitrator cannot withdraw, wait, re-stake and present the age they
    // held before the tokens left.
    require!(
        ctx.accounts
            .arbitrator_stake
            .meets_stake_age(now, ctx.accounts.fee_config.min_arbitrator_stake_age_secs),
        ErrorCode::ArbitratorStakeTooYoung
    );

    let dispute_case = &mut ctx.accounts.dispute_case;

    require!(
        now < dispute_case.commit_deadline,
        ErrorCode::CommitWindowClosed
    );

    // The draw, last of the three gates. Evaluated against `round_opened_at`
    // rather than `opened_at`, so a re-opened round draws over its own fresh
    // window instead of inheriting one that has already elapsed — which
    // would leave the threshold fully open from the first moment of every
    // round after the first.
    //
    // The threshold widens as the window elapses, so a wallet that does
    // not qualify now may qualify later in the same round. That is
    // required for liveness on a small pool, and is the reason this cannot
    // be checked once and cached.
    // A seat that went silent in an earlier round of this case does not
    // get another. Taking seats and never revealing is how a losing party
    // drives every round to no decision and collects the terminal even
    // split — see `DisputeCase::barred`.
    require!(
        !dispute_case.barred.contains(&ctx.accounts.arbitrator.key()),
        ErrorCode::ArbitratorBarredFromCase
    );

    require!(
        openfiat_programs_shared::sortition::qualifies_for_seat(
            &dispute_case.case_seed,
            &ctx.accounts.arbitrator_stake.key(),
            ctx.accounts.fee_config.arbitrator_sortition_bps,
            dispute_case.round_opened_at,
            dispute_case.commit_deadline,
            now,
        ),
        ErrorCode::NotDrawnForThisCase
    );
    require!(
        !dispute_case
            .arbitrators
            .contains(&ctx.accounts.arbitrator.key()),
        ErrorCode::AlreadyCommitted
    );
    require!(
        dispute_case.arbitrators.len() < MAX_ARBITRATORS,
        ErrorCode::DisputeCaseFull
    );

    dispute_case.arbitrators.push(ctx.accounts.arbitrator.key());
    dispute_case.commitments.push(commitment);
    dispute_case.revealed_outcomes.push(None);
    dispute_case.weights.push(0);
    // Survives the per-round arrays being cleared, which is the whole point:
    // it is the only record that a wallet ever served on this case once the
    // round it served in is gone. The pool floor uses it to refuse to believe
    // a published pool smaller than the participation this case has already
    // seen — see `DisputeCase::seats_taken_total`.
    dispute_case.seats_taken_total = dispute_case
        .seats_taken_total
        .checked_add(1)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}
