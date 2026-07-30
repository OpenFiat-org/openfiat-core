//! Deterministic swap-aggregator test fixture.
//!
//! Stands in for Jupiter's real aggregator program in CI, since cloning
//! live mainnet liquidity into `anchor test`'s ephemeral validator is
//! inherently flaky and not what we're actually trying to verify in this
//! test suite — presale's `contribute_with_swap` doesn't trust anything
//! about a swap program's internals (see that instruction's doc comment),
//! only that the configured `swap_program` id matches and that the
//! destination vault's balance increased by at least the required amount.
//! This program exercises exactly that CPI-forwarding/balance-delta
//! plumbing with two plain token transfers, standing in for "debit the
//! buyer's source asset, credit USDC out of a pre-funded reserve."
//!
//! The REAL Jupiter integration is exercised separately via the documented
//! devnet/mainnet-fork smoke test (see programs/README.md) — this fixture
//! makes no claim about Jupiter's actual routing/pricing behavior.
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};

declare_id!("8ysRdUzhhvm4jHJwbJqYF6ZuS2AqekfTJbxMUF1g3Ugn");

#[constant]
pub const RESERVE_AUTHORITY_SEED: &[u8] = b"reserve";

#[program]
pub mod mock_jupiter {
    use super::*;

    /// Debits `amount_in` from `source` (signed by `source_authority`,
    /// which must already be a signer on the outer transaction — this is
    /// exactly the shape a real aggregator CPI takes) and credits
    /// `amount_out` of a *different* mint into `destination`, drawn from
    /// `reserve` (pre-funded by the test setup, owned by this program's
    /// `reserve_authority` PDA).
    pub fn mock_swap(ctx: Context<MockSwap>, amount_in: u64, amount_out: u64) -> Result<()> {
        token_interface::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.source.to_account_info(),
                    mint: ctx.accounts.source_mint.to_account_info(),
                    to: ctx.accounts.sink.to_account_info(),
                    authority: ctx.accounts.source_authority.to_account_info(),
                },
            ),
            amount_in,
            ctx.accounts.source_mint.decimals,
        )?;

        let bump = ctx.bumps.reserve_authority;
        let signer_seeds: &[&[u8]] = &[RESERVE_AUTHORITY_SEED, &[bump]];
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    from: ctx.accounts.reserve.to_account_info(),
                    mint: ctx.accounts.destination_mint.to_account_info(),
                    to: ctx.accounts.destination.to_account_info(),
                    authority: ctx.accounts.reserve_authority.to_account_info(),
                },
                &[signer_seeds],
            ),
            amount_out,
            ctx.accounts.destination_mint.decimals,
        )?;
        Ok(())
    }
}

#[derive(Accounts)]
pub struct MockSwap<'info> {
    pub source_authority: Signer<'info>,

    #[account(mut)]
    pub source: InterfaceAccount<'info, TokenAccount>,
    /// Both mints are pinned to the single `token_program` below, which
    /// means this fixture can only swap between two mints owned by the
    /// *same* token program. A real aggregator would take one program per
    /// side (see [`openfiat_programs_shared::token_dispatch`]); the fixture
    /// does not, because widening it would be inventing behaviour the tests
    /// do not exercise. If a test ever needs a legacy-SPL/Token-2022 pair
    /// swapped, this is the account list that has to grow.
    #[account(mint::token_program = token_program)]
    pub source_mint: InterfaceAccount<'info, Mint>,
    #[account(mut)]
    pub sink: InterfaceAccount<'info, TokenAccount>,

    #[account(mut)]
    pub destination: InterfaceAccount<'info, TokenAccount>,
    #[account(mint::token_program = token_program)]
    pub destination_mint: InterfaceAccount<'info, Mint>,
    #[account(mut)]
    pub reserve: InterfaceAccount<'info, TokenAccount>,

    /// CHECK: verified via seeds/bump; signs the reserve->destination transfer.
    #[account(seeds = [RESERVE_AUTHORITY_SEED], bump)]
    pub reserve_authority: UncheckedAccount<'info>,

    pub token_program: Interface<'info, TokenInterface>,
}
