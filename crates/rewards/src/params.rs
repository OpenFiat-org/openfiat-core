//! The tunable half of OFS-4100 §9.2.
//!
//! Every value here is a governance parameter rather than a constant, so
//! none of them is baked into the arithmetic — [`RewardParams`] is
//! threaded through every computation and [`RewardParams::default`]
//! merely reproduces the specification's current values.
//!
//! Sign-off status is per field and tracked in OFS-4100 §9.2, not here in
//! bulk. Most of these are `[CONFIRMED]`; the two content-retrievability
//! multipliers are not, because they were added after that section's
//! sign-off round and §2 reserves the proposed tag for exactly that.
//! Governance-updatability is required either way — it is what stops a
//! later decision needing a code change.

/// Basis-point denominator, matching the on-chain programs' own
/// `BPS_DENOMINATOR` so a reader comparing the two sees one convention.
pub const BPS_DENOMINATOR: u64 = 10_000;

/// OPEN's decimal precision (OFS-4100 §1, re-baselined 2026-08-09 to 6
/// decimals), used only to express the defaults below in whole OPEN rather
/// than base units.
const OPEN: u64 = 1_000_000; // 6 decimals

/// §9.2's parameters, in the units the arithmetic actually uses:
/// milliseconds for time and base units for amounts, so no conversion
/// happens inside the money path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RewardParams {
    /// Length of one reward epoch. Also the window liveness is measured
    /// over. `[CONFIRMED]` 24 hours.
    pub epoch_millis: u64,

    /// How many slices the epoch is divided into for the availability
    /// measurement. A node scores one slice by being observed at all
    /// during it, so a chattier node earns nothing extra — see
    /// [`crate::liveness`] for why presence-per-slice rather than an
    /// event count is the honest metric here.
    pub availability_buckets: u32,

    /// Bootstrap emission per epoch, in base units, before the remaining
    /// bucket caps it. `[CONFIRMED]` ≈82,192 OPEN, being
    /// the 120,000,000 OPEN Infrastructure bucket spread linearly across
    /// four years.
    pub per_epoch_emission: u64,

    /// Multiplier for a node observed bridging to Solana.
    /// `[CONFIRMED]` 1.0.
    pub connectivity_rpc_bps: u64,

    /// Multiplier for a node observed only gossiping.
    /// `[CONFIRMED]` 0.4.
    pub connectivity_gossip_bps: u64,

    /// Multiplier for a node that answered a retrievability challenge —
    /// that is, proved it holds content the protocol references.
    /// `[PROPOSED — NEEDS SIGN-OFF]` 1.0.
    pub pinning_serving_bps: u64,

    /// Multiplier for a node that did not.
    /// `[PROPOSED — NEEDS SIGN-OFF]` 0.7, so a node that pins earns
    /// roughly 1.43× an otherwise identical node that does not.
    ///
    /// # Why the premium is expressed as a penalty
    ///
    /// "Nodes with IPFS earn more" and "nodes without IPFS earn less" are
    /// the same statement here, and only the second can be implemented.
    /// Emission per epoch is fixed, and these multipliers decide how it is
    /// *divided*; a bonus above 1.0 would not pay a pinning node extra out
    /// of thin air, it would mint emission that the Infrastructure bucket
    /// does not contain. [`RewardParams::validate`] rejects that outright.
    /// So the pinning node keeps its full share and the non-pinning node
    /// yields part of its own — which produces exactly the intended
    /// relative outcome without inventing tokens.
    ///
    /// # Why 0.7 rather than gossip-only's 0.4
    ///
    /// Storage is a smaller favour to the network than a chain
    /// connection. A `GossipOnly` node cannot answer an on-chain question
    /// at all, so every `RpcConnected` peer is carrying it; a node that
    /// does not pin still relays, validates and serves everything else,
    /// and the content it declines to hold is held by a gateway anyway.
    /// A penalty as steep as connectivity's would price storage as if it
    /// were the scarcer service, which it is not.
    pub pinning_absent_bps: u64,

    /// Stake at or below which a node earns nothing, in base units. This
    /// mirrors the on-chain `min_stake_by_role[NodeOperator]` rather than
    /// replacing it: the program is what actually enforces the floor at
    /// stake time, and this is the paying side declining to pay someone
    /// who has since fallen below it. `[CONFIRMED]` 1,000 OPEN, the
    /// deployed value.
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
            pinning_serving_bps: BPS_DENOMINATOR,
            pinning_absent_bps: 7_000,
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
            || self.pinning_serving_bps > BPS_DENOMINATOR
            || self.pinning_absent_bps > BPS_DENOMINATOR
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
        for spoiled in [
            RewardParams {
                connectivity_rpc_bps: BPS_DENOMINATOR + 1,
                ..RewardParams::default()
            },
            // The tempting way to write "pinning nodes earn more": a
            // bonus above 1.0. It does not pay anyone extra, it just
            // apportions emission the bucket does not hold.
            RewardParams {
                pinning_serving_bps: BPS_DENOMINATOR + 1,
                ..RewardParams::default()
            },
        ] {
            assert_eq!(spoiled.validate(), Err(InvalidParams::MultiplierAboveOne));
        }
    }

    #[test]
    fn pinning_is_worth_less_than_a_chain_connection() {
        // Both are penalties on the node that lacks the service, so a
        // smaller penalty means the service is priced lower. Storage is
        // the lesser favour — see `pinning_absent_bps`.
        let p = RewardParams::default();
        assert!(p.pinning_absent_bps > p.connectivity_gossip_bps);
    }
}
