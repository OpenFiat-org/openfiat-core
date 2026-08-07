pub mod constants;
pub mod error;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7");

/// `openfiat-presale` — the OPEN token presale program (OFS-4200 §3,
/// OFS-4100 §3). Phase 3: full sale lifecycle — initialize, contribute
/// (direct USDC or SOL/stablecoin via atomic Jupiter CPI swap), finalize,
/// claim. There is no refund path (soft_cap is forced to 0).
#[program]
pub mod presale {
    use super::*;

    pub fn initialize_sale(
        ctx: Context<InitializeSale>,
        sale_nonce: u64,
        params: InitializeSaleParams,
    ) -> Result<()> {
        crate::instructions::initialize_sale::handle_initialize_sale(ctx, sale_nonce, params)
    }

    pub fn contribute_usdc(
        ctx: Context<ContributeUsdc>,
        sale_nonce: u64,
        amount: u64,
    ) -> Result<()> {
        crate::instructions::contribute_usdc::handle_contribute_usdc(ctx, sale_nonce, amount)
    }

    pub fn contribute_with_swap(
        ctx: Context<ContributeWithSwap>,
        sale_nonce: u64,
        expected_usdc_out: u64,
        swap_instruction_data: Vec<u8>,
    ) -> Result<()> {
        crate::instructions::contribute_with_swap::handle_contribute_with_swap(
            ctx,
            sale_nonce,
            expected_usdc_out,
            swap_instruction_data,
        )
    }

    pub fn finalize_sale(ctx: Context<FinalizeSale>, sale_nonce: u64) -> Result<()> {
        crate::instructions::finalize_sale::handle_finalize_sale(ctx, sale_nonce)
    }

    pub fn claim(ctx: Context<Claim>, sale_nonce: u64) -> Result<()> {
        crate::instructions::claim::handle_claim(ctx, sale_nonce)
    }

    pub fn sweep_proceeds(
        ctx: Context<SweepProceeds>,
        sale_nonce: u64,
        amount: u64,
    ) -> Result<()> {
        crate::instructions::sweep_proceeds::handle_sweep_proceeds(ctx, sale_nonce, amount)
    }

    pub fn update_sale_params(
        ctx: Context<UpdateSaleParams>,
        sale_nonce: u64,
        params: UpdateSaleParamsArgs,
    ) -> Result<()> {
        crate::instructions::update_sale_params::handle_update_sale_params(ctx, sale_nonce, params)
    }
}
