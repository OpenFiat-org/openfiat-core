use anchor_lang::prelude::*;

use crate::constants::MAX_STABLECOINS;

/// Lifecycle state of the presale (OFS-4200 §3).
///
/// `Active -> Finalized` is the only transition out of `Active` and is
/// terminal. There is no refund path (soft_cap is forced to 0 at
/// initialization), so there is no `SoftCapMissed` state.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum SaleState {
    Active,
    Finalized,
}

/// Singleton sale configuration + running state (OFS-4200 §3, OFS-4100 §3).
///
/// All economic parameters below are set once at `initialize_sale` from
/// OFS-4100 §3's PROPOSED figures — they are instruction arguments, not
/// compile-time constants, specifically so a later tokenomics sign-off
/// (or a devnet-vs-mainnet difference) never requires a code change, only a
/// different `initialize_sale` call.
#[account]
#[derive(InitSpace)]
pub struct SaleConfig {
    pub admin: Pubkey,
    /// The OPEN token mint (fixed supply, genesis-minted — see Phase 2).
    pub open_mint: Pubkey,
    /// The USDC mint contributions are ultimately valued/held in.
    pub usdc_mint: Pubkey,
    /// Community Presale allocation bucket (OFS-4100 §2), owned by the
    /// `presale_vault` PDA — `claim` transfers out of this account.
    pub presale_vault: Pubkey,
    /// USDC escrow token account (owned by this `SaleConfig` PDA itself)
    /// that holds contributions until `finalize_sale` sweeps them to
    /// `treasury`.
    pub usdc_vault: Pubkey,
    /// Destination for collected USDC once the sale finalizes successfully.
    pub treasury: Pubkey,
    /// The trusted swap-aggregator program CPI'd into by `contribute_with_swap`.
    /// Production devnet/mainnet deployments must set this to Jupiter's real,
    /// verified aggregator program id; test/CI deployments may point it at a
    /// deterministic mock so the swap-forwarding logic is testable without a
    /// live, flaky mainnet-state clone. See `contribute_with_swap` for why
    /// this is safe regardless of which program is configured here: the
    /// *result* (a verified balance increase in `usdc_vault`) is what's
    /// trusted, not any account layout internal to this program.
    pub swap_program: Pubkey,
    /// USDC base units (6 decimals).
    ///
    /// OFS-4100 §3 gives the presale no hard cap distinct from the Community
    /// Presale bucket itself, so the confirmed value is the full bucket:
    /// 20,000,000,000 OPEN at the configured `open_per_usdc` rate (100, i.e.
    /// 1 USDC = 100 OPEN — re-baselined 2026-08-09). It must never be set
    /// higher — `claim` pays out of a vault holding exactly that much OPEN,
    /// and entitlements accrue against
    /// contributions at that rate, so a larger cap would let the sale sell
    /// OPEN the vault cannot deliver.
    pub hard_cap: u64,
    /// USDC base units. **Always zero**: OFS-4100 §3 confirms there is no
    /// soft cap and no refund condition derived from one, and
    /// `initialize_sale`/`update_sale_params` both reject a non-zero value
    /// with `ErrorCode::SoftCapNotSupported`. Zero is how "no minimum to
    /// raise" is expressed here — `finalize_sale` always resolves to
    /// `Finalized`.
    pub soft_cap: u64,
    /// USDC base units, applies to a wallet's first contribution only.
    pub min_contribution: u64,
    /// USDC base units, applies to a wallet's cumulative contributions.
    /// OFS-4100 §3: 10,000,000 USDC-equivalent.
    pub max_contribution: u64,
    /// Basis points; a swap whose realized output falls below
    /// `expected_out * (10_000 - max_slippage_bps) / 10_000` is rejected.
    pub max_slippage_bps: u16,
    /// OPEN base units credited per 1 USDC base unit's worth of contribution,
    /// as a whole multiplier (100 for the presale, 80 for the public-sale
    /// phase). Set at initialize_sale; > 0 enforced.
    pub open_per_usdc: u64,
    /// Cached from `open_mint`/`usdc_mint` at `initialize_sale` so later
    /// instructions don't need to pass the mint accounts just to read
    /// decimals. `initialize_sale` requires open_decimals >= usdc_decimals
    /// so the USDC->OPEN scale-up below never underflows.
    pub open_decimals: u8,
    pub usdc_decimals: u8,
    pub start_time: i64,
    pub end_time: i64,
    #[max_len(MAX_STABLECOINS)]
    pub stablecoin_whitelist: Vec<Pubkey>,
    /// Running total of USDC-equivalent raised, in USDC base units.
    pub total_raised: u64,
    pub state: SaleState,
    pub bump: u8,
    pub usdc_vault_bump: u8,
}

impl SaleConfig {
    /// Converts a USDC base-unit amount into the OPEN base-unit entitlement
    /// at the configured `open_per_usdc` rate (OFS-4100 §3), scaling for the
    /// two mints' decimal difference. `initialize_sale` requires
    /// `open_decimals >= usdc_decimals`, so the decimal scale-up never
    /// underflows.
    pub fn open_entitlement_for(&self, usdc_amount: u64) -> Result<u64> {
        let scale = 10u64
            .checked_pow((self.open_decimals - self.usdc_decimals) as u32)
            .ok_or(crate::error::ErrorCode::Overflow)?;
        usdc_amount
            .checked_mul(scale)
            .ok_or(crate::error::ErrorCode::Overflow)?
            .checked_mul(self.open_per_usdc)
            .ok_or(crate::error::ErrorCode::Overflow.into())
    }
}

/// Per-buyer contribution record: PDA seeds `[CONTRIBUTION_SEED, sale_config, buyer]`.
#[account]
#[derive(InitSpace)]
pub struct Contribution {
    pub buyer: Pubkey,
    /// Cumulative USDC-equivalent contributed by this wallet, base units.
    pub amount_usdc: u64,
    /// OPEN base units this wallet is entitled to claim (`amount_usdc`
    /// converted via `SaleConfig::open_entitlement_for` at the configured
    /// `open_per_usdc` rate — OFS-4100 §3 confirms no presale vesting).
    pub open_entitlement: u64,
    /// OPEN base units already claimed. Monotonic high-water mark: a claim
    /// pays `open_entitlement - claimed_open`, so a buyer who contributes
    /// again after claiming can claim only the newly-accrued delta.
    pub claimed_open: u64,
    pub bump: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimally valid `SaleConfig` for unit-testing pure helper methods.
    /// Callers override only the fields their test cares about via `..
    /// test_config()`.
    fn test_config() -> SaleConfig {
        SaleConfig {
            admin: Pubkey::default(),
            open_mint: Pubkey::default(),
            usdc_mint: Pubkey::default(),
            presale_vault: Pubkey::default(),
            usdc_vault: Pubkey::default(),
            treasury: Pubkey::default(),
            swap_program: Pubkey::default(),
            hard_cap: 0,
            soft_cap: 0,
            min_contribution: 0,
            max_contribution: 0,
            max_slippage_bps: 0,
            open_per_usdc: 0,
            open_decimals: 6,
            usdc_decimals: 6,
            start_time: 0,
            end_time: 0,
            stablecoin_whitelist: Vec::new(),
            total_raised: 0,
            state: SaleState::Active,
            bump: 0,
            usdc_vault_bump: 0,
        }
    }

    #[test]
    fn entitlement_applies_rate_at_matching_decimals() {
        let c = SaleConfig {
            open_decimals: 6,
            usdc_decimals: 6,
            open_per_usdc: 100,
            ..test_config()
        };
        // 1 USDC (1e6) * scale(1) * rate(100) = 100 OPEN (1e8 base units @ 6 dec)
        assert_eq!(c.open_entitlement_for(1_000_000).unwrap(), 100_000_000);
    }

    #[test]
    fn entitlement_boundary_max_contribution_no_overflow() {
        let c = SaleConfig {
            open_decimals: 6,
            usdc_decimals: 6,
            open_per_usdc: 100,
            ..test_config()
        };
        // $10M USDC = 1e13 base units * 100 = 1e15 < u64::MAX
        assert_eq!(
            c.open_entitlement_for(10_000_000_000_000).unwrap(),
            1_000_000_000_000_000
        );
    }
}
