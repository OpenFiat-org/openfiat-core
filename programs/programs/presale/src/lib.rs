pub mod constants;
pub mod error;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7");

/// `openfiat-presale` — the OPEN token presale program (OFS-4200 §3).
///
/// This is Phase 1 scaffolding only: a single `initialize` instruction that
/// creates an empty `SaleConfig` singleton, proving the workspace builds,
/// deploys, and tests end-to-end. The real `contribute`/`finalize_sale`/
/// `claim`/`refund` instructions and the Jupiter CPI swap path land in
/// Phase 3, once OFS-4100's presale terms are signed off.
#[program]
pub mod presale {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        crate::instructions::initialize::handle_initialize(ctx)
    }
}
