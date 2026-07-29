//! The tunable half of OFS-4100 §9.2.
//!
//! Every value here is `[PROPOSED — NEEDS SIGN-OFF]` in the specification
//! and a governance parameter rather than a constant, so none of them is
//! baked into the arithmetic — [`RewardParams`] is threaded through every
//! computation and [`RewardParams::default`] merely reproduces the
//! specification's current starting point.

/// Basis-point denominator, matching the on-chain programs' own
/// `BPS_DENOMINATOR` so a reader comparing the two sees one convention.
pub const BPS_DENOMINATOR: u64 = 10_000;

/// OPEN's decimal precision (OFS-4100 §1), used only to express the
/// defaults below in whole OPEN rather than base units.
const OPEN: u64 = 1_000_000_000;

/// §9.2's parameters, in the units the arithmetic actually uses:
/// milliseconds for time and base units for amounts, so no conversion
/// happens inside the money path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RewardParams {
    /// Length of one reward epoch. Also the window liveness is measured
    /// over. `[PROPOSED — NEEDS SIGN-OFF]` 24 hours.
    pub epoch_millis: u64,

    /// How many slices the epoch is divided into for the availability
    /// measurement. A node scores one slice by being observed at all
    /// during it, so a chattier node earns nothing extra — see
    /// [`crate::liveness`] for why presence-per-slice rather than an
    /// event count is the honest metric here.
    pub availability_buckets: u32,

    /// Bootstrap emission per epoch, in base units, before the remaining
    /// bucket caps it. `[PROPOSED — NEEDS SIGN-OFF]` ≈82,192 OPEN, being
    /// the 120,000,000 OPEN Infrastructure bucket spread linearly across
    /// four years.
    pub per_epoch_emission: u64,

    /// Multiplier for a node observed bridging to Solana.
    /// `[PROPOSED — NEEDS SIGN-OFF]` 1.0.
    pub connectivity_rpc_bps: u64,

    /// Multiplier for a node observed only gossiping.
    /// `[PROPOSED — NEEDS SIGN-OFF]` 0.4.
    pub connectivity_gossip_bps: u64,

    /// Stake at or below which a node earns nothing, in base units. This
    /// mirrors the on-chain `min_stake_by_role[NodeOperator]` rather than
    /// replacing it: the program is what actually enforces the floor at
    /// stake time, and this is the paying side declining to pay someone
    /// who has since fallen below it. `[PROPOSED — NEEDS SIGN-OFF]`
    /// 1,000 OPEN, the deployed value.
    pub min_stake: u64,
}

impl Default for RewardParams {
    fn default() -> Self {
        Self {
            epoch_millis: 24 * 60 * 60 * 1_000,
            availability_buckets: 24,
            per_epoch_emission: 82_192 * OPEN,
            connectivity_rpc_bps: BPS_DENOMINATOR,
            connectivity_gossip_bps: 4_000,
            min_stake: 1_000 * OPEN,
        }
    }
}

impl RewardParams {
    /// Which epoch a timestamp falls in. Deterministic and stateless:
    /// every node computing a schedule for the same instant agrees on the
    /// epoch without coordinating, which is what lets the result be
    /// checked by someone other than whoever gets paid.
    pub fn epoch_index(&self, at: openfiat_types::Timestamp) -> u64 {
        at.as_millis() / self.epoch_millis.max(1)
    }

    /// The inclusive-start, exclusive-end millisecond bounds of `epoch`.
    pub fn epoch_bounds(&self, epoch: u64) -> (u64, u64) {
        let start = epoch.saturating_mul(self.epoch_millis);
        (start, start.saturating_add(self.epoch_millis))
    }

    /// Rejects a parameter set whose arithmetic would be meaningless,
    /// rather than letting a zero denominator surface later as a silent
    /// zero payout.
    pub fn validate(&self) -> Result<(), InvalidParams> {
        if self.epoch_millis == 0 {
            return Err(InvalidParams::ZeroEpoch);
        }
        if self.availability_buckets == 0 {
            return Err(InvalidParams::ZeroBuckets);
        }
        if self.connectivity_rpc_bps > BPS_DENOMINATOR
            || self.connectivity_gossip_bps > BPS_DENOMINATOR
        {
            return Err(InvalidParams::MultiplierAboveOne);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidParams {
    ZeroEpoch,
    ZeroBuckets,
    MultiplierAboveOne,
}

impl std::fmt::Display for InvalidParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroEpoch => write!(f, "epoch_millis must be non-zero"),
            Self::ZeroBuckets => write!(f, "availability_buckets must be non-zero"),
            Self::MultiplierAboveOne => {
                write!(f, "a connectivity multiplier above 1.0 would mint emission")
            }
        }
    }
}

impl std::error::Error for InvalidParams {}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_types::Timestamp;

    #[test]
    fn the_specifications_defaults_are_self_consistent() {
        assert!(RewardParams::default().validate().is_ok());
    }

    #[test]
    fn epoch_index_is_stable_within_an_epoch_and_advances_across_one() {
        let p = RewardParams::default();
        let (start, end) = p.epoch_bounds(100);
        assert_eq!(p.epoch_index(Timestamp::from_millis(start)), 100);
        assert_eq!(p.epoch_index(Timestamp::from_millis(end - 1)), 100);
        assert_eq!(p.epoch_index(Timestamp::from_millis(end)), 101);
    }

    #[test]
    fn a_multiplier_above_one_is_rejected_because_it_would_mint_emission() {
        let p = RewardParams {
            connectivity_rpc_bps: BPS_DENOMINATOR + 1,
            ..RewardParams::default()
        };
        assert_eq!(p.validate(), Err(InvalidParams::MultiplierAboveOne));
    }
}
