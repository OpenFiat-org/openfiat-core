//! Fund-movement helpers shared by more than one instruction — kept out
//! of any single instruction file so `cancel_reservation` and
//! `expire_reservation` (identical unwind logic, different callers)
//! don't duplicate the CPI/accounting.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    transfer_checked, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use openfiat_programs_shared::VaultState;

use crate::constants::{BPS_DENOMINATOR, TRADE_ESCROW_SEED};
use crate::{error::ErrorCode, state::*};

/// Releases a reservation-marking without ever having funded a trade
/// escrow (e.g. cancelled/expired before `fund_trade_escrow` ran).
pub fn release_reservation_marking(
    liquidity_vault: &mut Account<LiquidityVault>,
    amount: u64,
) -> Result<()> {
    require!(
        liquidity_vault.reserved >= amount,
        ErrorCode::InsufficientReservedLiquidity
    );
    liquidity_vault.reserved -= amount;
    liquidity_vault.available = liquidity_vault
        .available
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}

/// Reverses a `fund_trade_escrow` — returns an already-funded trade
/// escrow's tokens to the liquidity vault's `available` balance. Used by
/// both `cancel_reservation` and `expire_reservation` for the case where
/// the escrow had already been funded before it was unwound.
#[allow(clippy::too_many_arguments)]
pub fn unwind_funded_trade_escrow<'info>(
    trade_escrow: &Account<'info, TradeEscrowVault>,
    trade_escrow_token_vault: &InterfaceAccount<'info, TokenAccount>,
    liquidity_vault: &mut Account<'info, LiquidityVault>,
    liquidity_token_vault: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    token_program: &Interface<'info, TokenInterface>,
) -> Result<()> {
    let amount = trade_escrow.amount;
    let id_bytes = trade_escrow.reservation_id.to_le_bytes();
    let bump = trade_escrow.bump;
    let signer_seeds: &[&[u8]] = &[TRADE_ESCROW_SEED, &id_bytes, &[bump]];

    transfer_checked(
        CpiContext::new_with_signer(
            token_program.key(),
            TransferChecked {
                from: trade_escrow_token_vault.to_account_info(),
                mint: mint.to_account_info(),
                to: liquidity_token_vault.to_account_info(),
                authority: trade_escrow.to_account_info(),
            },
            &[signer_seeds],
        ),
        amount,
        mint.decimals,
    )?;

    liquidity_vault.pending_settlement -= amount;
    liquidity_vault.available = liquidity_vault
        .available
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}

/// Splits a funded trade escrow evenly between the buyer and the seller's
/// liquidity vault, taking no settlement fee.
///
/// Used for two outcomes that must not hand either party the whole
/// amount: a `MutualSettlement` verdict, and the terminal case where
/// arbitration has failed to reach a decision after
/// [`MAX_DISPUTE_ROUNDS`](crate::constants::MAX_DISPUTE_ROUNDS).
///
/// Even, rather than by some agreed ratio, because no ratio is recorded
/// anywhere on-chain — OFS-2400 §17's "Partial Settlement" is explicitly
/// future work. The even split is `[PROPOSED — NEEDS SIGN-OFF]`; what is
/// *not* negotiable is that neither of these cases may pay one side in
/// full, since that is precisely what made forcing indecision profitable.
///
/// No fee is charged: the protocol did not successfully arbitrate, so it
/// has not earned one. Also `[PROPOSED — NEEDS SIGN-OFF]`.
///
/// An odd unit goes to the seller's vault, chosen only so the division is
/// deterministic rather than for any economic reason.
#[allow(clippy::too_many_arguments)]
pub fn split_trade_escrow_evenly<'info>(
    trade_escrow: &mut Account<'info, TradeEscrowVault>,
    trade_escrow_token_vault: &InterfaceAccount<'info, TokenAccount>,
    buyer_token_account: &InterfaceAccount<'info, TokenAccount>,
    liquidity_vault: &mut Account<'info, LiquidityVault>,
    liquidity_token_vault: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    token_program: &Interface<'info, TokenInterface>,
) -> Result<()> {
    let amount = trade_escrow.amount;
    let buyer_share = amount / 2;
    let seller_share = amount.checked_sub(buyer_share).ok_or(ErrorCode::Overflow)?;

    let id_bytes = trade_escrow.reservation_id.to_le_bytes();
    let bump = trade_escrow.bump;
    let signer_seeds: &[&[u8]] = &[TRADE_ESCROW_SEED, &id_bytes, &[bump]];

    let decimals = mint.decimals;
    let token_program_id = token_program.key();
    let mint_info = mint.to_account_info();
    let from = trade_escrow_token_vault.to_account_info();
    let authority = trade_escrow.to_account_info();

    let destinations: [(AccountInfo<'info>, u64); 2] = [
        (buyer_token_account.to_account_info(), buyer_share),
        (liquidity_token_vault.to_account_info(), seller_share),
    ];

    for (to, share_amount) in destinations {
        if share_amount == 0 {
            continue;
        }
        transfer_checked(
            CpiContext::new_with_signer(
                token_program_id,
                TransferChecked {
                    from: from.clone(),
                    mint: mint_info.clone(),
                    to,
                    authority: authority.clone(),
                },
                &[signer_seeds],
            ),
            share_amount,
            decimals,
        )?;
    }

    trade_escrow.state = VaultState::Released;
    liquidity_vault.pending_settlement -= amount;
    liquidity_vault.available = liquidity_vault
        .available
        .checked_add(seller_share)
        .ok_or(ErrorCode::Overflow)?;
    liquidity_vault.settled = liquidity_vault
        .settled
        .checked_add(buyer_share)
        .ok_or(ErrorCode::Overflow)?;
    Ok(())
}

/// Splits `amount` into (buyer_amount, [dev, ecosystem, infra, emergency]),
/// per `FeeConfig`'s settlement fee rate and 4-way basis-point split.
/// Shared by `release_escrow` and `execute_dispute_outcome`'s `BuyerWins`
/// path — both release funds identically once a release is authorized.
pub fn compute_fee_split(fee_config: &FeeConfig, amount: u64) -> Result<(u64, [u64; 4])> {
    let fee = (amount as u128)
        .checked_mul(fee_config.settlement_fee_bps as u128)
        .ok_or(ErrorCode::Overflow)?
        .checked_div(BPS_DENOMINATOR as u128)
        .ok_or(ErrorCode::Overflow)? as u64;
    let buyer_amount = amount.checked_sub(fee).ok_or(ErrorCode::Overflow)?;

    Ok((buyer_amount, split_fee_four_ways(fee_config, fee)?))
}

/// Index of the ecosystem treasury within the four-way share array, which
/// every caller unpacks in the same order: `[dev, ecosystem, infra,
/// emergency]`.
///
/// Named rather than written as a bare `1` because that literal is the
/// entire difference between paying the rounding remainder to the right
/// treasury and paying it to a neighbouring one — a change that alters
/// where protocol income lands while looking like an off-by-one.
const ECOSYSTEM_TREASURY_SHARE: usize = 1;

/// Divides an already-determined fee across the four treasury
/// sub-accounts by `FeeConfig`'s basis-point split.
///
/// Separate from [`compute_fee_split`] because not every fee is a
/// percentage of a trade: the ad-listing fee is a flat amount that is
/// nonetheless protocol revenue and routes exactly the same way. Both
/// callers share this so the two can never drift apart — a listing fee
/// that split differently from a settlement fee would be a silent
/// accounting discrepancy rather than a visible bug.
///
/// Integer division truncates, so the four shares can sum to less than
/// `fee` by up to three sub-units. Per OFS-4100 §6 that remainder goes to
/// the **ecosystem** treasury. The amount is negligible by construction;
/// what is not negligible is that the spec and the program agree about
/// where protocol income lands. This routed to the emergency reserve
/// until OFS-4100 §6 was signed off, which is the same class of quiet
/// divergence that let the `FeeConfig` treasury addresses and staking's
/// `slash_destination` stay wrong for as long as they did — nobody
/// noticed, because nothing was ever compared.
pub fn split_fee_four_ways(fee_config: &FeeConfig, fee: u64) -> Result<[u64; 4]> {
    let splits = [
        fee_config.dev_treasury_bps,
        fee_config.ecosystem_treasury_bps,
        fee_config.infra_treasury_bps,
        fee_config.emergency_reserve_bps,
    ];
    let mut shares = [0u64; 4];
    let mut allocated = 0u64;
    for (i, bps) in splits.iter().enumerate() {
        let share = (fee as u128)
            .checked_mul(*bps as u128)
            .ok_or(ErrorCode::Overflow)?
            .checked_div(BPS_DENOMINATOR as u128)
            .ok_or(ErrorCode::Overflow)? as u64;
        shares[i] = share;
        allocated = allocated.checked_add(share).ok_or(ErrorCode::Overflow)?;
    }
    // Rounding remainder (basis-point division truncates) goes to the
    // ecosystem treasury rather than being silently lost — OFS-4100 §6.
    let remainder = fee.checked_sub(allocated).ok_or(ErrorCode::Overflow)?;
    shares[ECOSYSTEM_TREASURY_SHARE] = shares[ECOSYSTEM_TREASURY_SHARE]
        .checked_add(remainder)
        .ok_or(ErrorCode::Overflow)?;

    Ok(shares)
}

/// Moves an approved (or dispute-resolved `BuyerWins`) trade escrow's
/// funds to the buyer plus the fee split's treasury destinations, and
/// updates `liquidity_vault`'s settled/pending counters. Shared by
/// `release_escrow` and `execute_dispute_outcome`.
#[allow(clippy::too_many_arguments)]
pub fn release_trade_escrow_funds<'info>(
    trade_escrow: &mut Account<'info, TradeEscrowVault>,
    trade_escrow_token_vault: &InterfaceAccount<'info, TokenAccount>,
    buyer_token_account: &InterfaceAccount<'info, TokenAccount>,
    liquidity_vault: &mut Account<'info, LiquidityVault>,
    fee_config: &FeeConfig,
    dev_treasury: &InterfaceAccount<'info, TokenAccount>,
    ecosystem_treasury: &InterfaceAccount<'info, TokenAccount>,
    infra_treasury: &InterfaceAccount<'info, TokenAccount>,
    emergency_reserve: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    token_program: &Interface<'info, TokenInterface>,
) -> Result<(u64, [u64; 4])> {
    let amount = trade_escrow.amount;
    let (buyer_amount, fee_shares) = compute_fee_split(fee_config, amount)?;

    let id_bytes = trade_escrow.reservation_id.to_le_bytes();
    let bump = trade_escrow.bump;
    let signer_seeds: &[&[u8]] = &[TRADE_ESCROW_SEED, &id_bytes, &[bump]];

    let decimals = mint.decimals;
    let token_program_id = token_program.key();
    let mint_info = mint.to_account_info();
    let from = trade_escrow_token_vault.to_account_info();
    let authority = trade_escrow.to_account_info();

    let destinations: [(AccountInfo<'info>, u64); 5] = [
        (buyer_token_account.to_account_info(), buyer_amount),
        (dev_treasury.to_account_info(), fee_shares[0]),
        (ecosystem_treasury.to_account_info(), fee_shares[1]),
        (infra_treasury.to_account_info(), fee_shares[2]),
        (emergency_reserve.to_account_info(), fee_shares[3]),
    ];

    for (to, share_amount) in destinations {
        if share_amount == 0 {
            continue;
        }
        transfer_checked(
            CpiContext::new_with_signer(
                token_program_id,
                TransferChecked {
                    from: from.clone(),
                    mint: mint_info.clone(),
                    to,
                    authority: authority.clone(),
                },
                &[signer_seeds],
            ),
            share_amount,
            decimals,
        )?;
    }

    trade_escrow.state = VaultState::Released;
    liquidity_vault.pending_settlement -= amount;
    liquidity_vault.settled = liquidity_vault
        .settled
        .checked_add(amount)
        .ok_or(ErrorCode::Overflow)?;
    Ok((buyer_amount, fee_shares))
}

/// Latches a fresh sortition seed for a dispute round from the most recent
/// entry in the slot-hashes sysvar (OFS-4100 §4.1).
///
/// `slot_hashes` must be the real sysvar; every caller pins it with an
/// `address = SlotHashes::id()` constraint, so this function is only ever
/// handed the one account it can be. The sysvar is read as raw bytes
/// because it is far too large to deserialize inside a program — and
/// `Sysvar::get()` is unsupported for it for the same reason.
///
/// Layout: an 8-byte little-endian entry count, then that many
/// `(slot: u64, hash: [u8; 32])` records, most recent first.
///
/// # What this achieves, and what it does not
///
/// It stops wallets being **pre-ground** against a known seed. An attacker
/// who wants many wallets to qualify for a specific future case cannot
/// compute their draws in advance, because the slot hash does not exist
/// until the block does — and the stake-age requirement means they would
/// have needed to commit the capital thirty days earlier anyway.
///
/// It does **not** stop the submitter from grinding. Whoever sends the
/// transaction can simulate it, see the resulting draw, and resubmit in a
/// later slot until the seed suits them. For `open_dispute_case` that is a
/// party to the trade. Solana offers no in-transaction randomness, so no
/// single-transaction seed can close this; a VRF or a two-transaction
/// future-slot commit is the real fix, and is tracked as follow-up rather
/// than implied here. Grinding does not remove the barrier sortition
/// exists to create — an attacker still needs many aged, funded wallets
/// before any draw can land well — it lowers how many.
///
/// The reservation id and escrow address are mixed in so two cases opening
/// in the same slot cannot share a seed and therefore a draw.
pub fn latch_case_seed(
    slot_hashes: &AccountInfo,
    reservation_id: u64,
    trade_escrow: &Pubkey,
) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};

    let data = slot_hashes.try_borrow_data()?;
    require!(data.len() >= 8 + 40, ErrorCode::SlotHashesUnavailable);
    let mut count_bytes = [0u8; 8];
    count_bytes.copy_from_slice(&data[..8]);
    // An empty sysvar would mean reading the "most recent" entry out of
    // padding, producing a seed an attacker could predict as a constant.
    require!(
        u64::from_le_bytes(count_bytes) > 0,
        ErrorCode::SlotHashesUnavailable
    );

    let mut hasher = Sha256::new();
    hasher.update(b"openfiat-dispute-case-seed");
    hasher.update(&data[8..8 + 40]); // most recent (slot, hash) pair
    hasher.update(reservation_id.to_le_bytes());
    hasher.update(trade_escrow.as_ref());
    Ok(hasher.finalize().into())
}

/// The fee split is the one piece of this file that is pure arithmetic
/// over a config, so it is the one piece that can be pinned by a unit
/// test rather than only by a devnet run.
///
/// What these assert is not that the numbers are large — they are dust —
/// but that the four shares always sum back to the fee and that the
/// leftover lands where OFS-4100 §6 says it does. A split that quietly
/// loses a sub-unit, or routes it to the wrong treasury, produces books
/// that never balance and a discrepancy nobody can see from the outside.
#[cfg(test)]
mod tests {
    use super::*;

    /// A `FeeConfig` carrying only the fields the split reads. Everything
    /// else is zeroed: naming the four bps values explicitly at each call
    /// site is what makes each test's arithmetic checkable by eye.
    fn config(dev: u16, ecosystem: u16, infra: u16, emergency: u16) -> FeeConfig {
        FeeConfig {
            admin: Pubkey::default(),
            ad_listing_fee: 0,
            dispute_filing_fee: 0,
            settlement_fee_bps: 0,
            dev_treasury: Pubkey::default(),
            ecosystem_treasury: Pubkey::default(),
            infra_treasury: Pubkey::default(),
            emergency_reserve: Pubkey::default(),
            dev_treasury_bps: dev,
            ecosystem_treasury_bps: ecosystem,
            infra_treasury_bps: infra,
            emergency_reserve_bps: emergency,
            timeout_secs: 0,
            bump: 0,
            min_arbitrator_stake_age_secs: 0,
            arbitrator_sortition_bps: 0,
            settlement_mints: [Pubkey::default(); crate::constants::MAX_SETTLEMENT_MINTS],
            settlement_mint_count: 0,
        }
    }

    /// The deployed 40/30/20/10 split, on a fee that divides exactly —
    /// no remainder exists, so nobody gets a bonus sub-unit.
    #[test]
    fn an_exact_split_gives_each_treasury_its_own_share_only() {
        let shares = split_fee_four_ways(&config(4_000, 3_000, 2_000, 1_000), 1_000).unwrap();
        assert_eq!(shares, [400, 300, 200, 100]);
    }

    /// The point of the change: truncation dust goes to the ecosystem
    /// treasury (index 1), not the emergency reserve (index 3).
    #[test]
    fn the_truncation_remainder_goes_to_the_ecosystem_treasury() {
        // 40/30/20/10 of 7 truncates to 2 + 2 + 1 + 0 = 5, leaving 2.
        let shares = split_fee_four_ways(&config(4_000, 3_000, 2_000, 1_000), 7).unwrap();
        assert_eq!(shares, [2, 4, 1, 0]);
    }

    /// A fee too small for any share to round above zero still has to
    /// arrive somewhere — the whole thing is remainder.
    #[test]
    fn a_fee_that_rounds_every_share_to_zero_lands_entirely_on_the_ecosystem() {
        let shares = split_fee_four_ways(&config(4_000, 3_000, 2_000, 1_000), 1).unwrap();
        assert_eq!(shares, [0, 1, 0, 0]);
    }

    /// A zero ecosystem allocation must not turn the remainder into a
    /// leak: the treasury still receives dust it has no bps share of,
    /// because the alternative is a fee that does not add up.
    #[test]
    fn the_remainder_still_lands_when_the_ecosystem_share_is_zero() {
        // 50/0/30/20 of 3 truncates to 1 + 0 + 0 + 0, leaving 2.
        let shares = split_fee_four_ways(&config(5_000, 0, 3_000, 2_000), 3).unwrap();
        assert_eq!(shares, [1, 2, 0, 0]);
    }

    /// The invariant behind all of the above, across a range of fees and
    /// two different splits: nothing is created and nothing is lost.
    #[test]
    fn the_four_shares_always_sum_to_the_fee() {
        for fee_config in [
            config(4_000, 3_000, 2_000, 1_000),
            config(3_333, 3_333, 3_333, 1),
        ] {
            for fee in 0..1_000u64 {
                let shares = split_fee_four_ways(&fee_config, fee).unwrap();
                assert_eq!(
                    shares.iter().sum::<u64>(),
                    fee,
                    "shares {shares:?} did not sum to {fee}"
                );
            }
        }
    }
}
