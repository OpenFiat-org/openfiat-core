//! Turning a [`PricingModel`] into an actual number (§11).
//!
//! # Where a floating price is allowed to exist
//!
//! A floating price is a function of a rate that moves continuously, so it
//! is only meaningful at an instant. That leaves exactly one safe place to
//! compute it — at the moment somebody asks — and one safe place to
//! *keep* it: whatever record represents the commitment. It is deliberately
//! never written back onto the [`crate::Advertisement`], which replicates
//! by gossip: a price refreshed onto a replicated record by each node's own
//! clock and own oracle view is both stale between refreshes and different
//! on every node.
//!
//! So this module is a pure function. It takes a rate someone else looked
//! up and returns a number; it reads no clock, no store and no network, and
//! two nodes handed the same [`MidPrice`] always produce the same
//! [`PriceQuote`]. That is what makes the result safe to pin into a
//! commitment later — a quote a node computed from ambient state could not
//! be reproduced or checked by anyone else.

use crate::record::PricingModel;
use openfiat_types::{Amount, Timestamp};

/// An oracle mid-price to resolve against, or why there isn't one.
///
/// The caller does the lookup (this crate deliberately does not depend on
/// `openfiat-oracles`) and reports the outcome faithfully, including the
/// failures — an unpriceable advertisement is a real answer, not an error
/// to paper over with a fallback. There is no "last known rate" variant on
/// purpose: quoting a lapsed rate is the failure this whole path exists to
/// prevent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MidPrice {
    /// A current median, good until `expires_at`.
    Available { rate: f64, expires_at: Timestamp },
    /// No provider publishes this advertisement's pair.
    NoOracleData,
    /// The pair is published but every record for it has expired.
    StaleOracleData,
}

/// Why an advertisement has no price at the instant it was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UnpriceableReason {
    NoOracleData,
    StaleOracleData,
    /// The premium puts the price outside what an [`Amount`] can hold —
    /// negative (a premium below `-10_000` bps), or past `u64::MAX` base
    /// units. Refused rather than saturated: a clamped price is a number
    /// nobody agreed to.
    PriceOutOfRange,
}

/// An advertisement's price at one instant, or the reason it has none.
///
/// `Fixed` and `Floating` are tagged distinctly all the way out to the
/// client rather than collapsed into a bare number, because they are not
/// the same promise: a taker choosing between two advertisements needs to
/// know which of the two can move between reading it and committing to it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum PriceQuote {
    /// A merchant-set price. Cannot fail to resolve, and moves only when
    /// the merchant signs a new one.
    Fixed { price: Amount },
    /// Oracle mid-price plus the merchant's premium. Good only until
    /// `mid_expires_at`, and liable to change before then.
    Floating {
        price: Amount,
        mid_rate: f64,
        premium_bps: i32,
        /// When the mid-price behind this number lapses — how long a
        /// caller may treat the quote as live.
        mid_expires_at: Timestamp,
    },
    /// Floating, but not priceable right now. Carries the premium anyway
    /// so a client can still show the ad's terms while being explicit that
    /// it currently has no price.
    Unpriceable {
        reason: UnpriceableReason,
        premium_bps: i32,
    },
}

impl PriceQuote {
    /// The resolved price, if there is one. `None` is a floating ad with
    /// no usable oracle read — never a zero, and never a stale number.
    pub const fn price(&self) -> Option<Amount> {
        match self {
            Self::Fixed { price } | Self::Floating { price, .. } => Some(*price),
            Self::Unpriceable { .. } => None,
        }
    }

    /// Whether this number can move without the merchant signing anything.
    pub const fn is_floating(&self) -> bool {
        !matches!(self, Self::Fixed { .. })
    }
}

impl PricingModel {
    /// Resolve this pricing model against an oracle read.
    ///
    /// `mid` is ignored entirely for [`PricingModel::Fixed`], so a caller
    /// need not perform a lookup for a fixed-price advertisement.
    pub fn quote(&self, mid: MidPrice) -> PriceQuote {
        match self {
            Self::Fixed { price } => PriceQuote::Fixed { price: *price },
            Self::Floating {
                premium_bps,
                price_decimals,
                ..
            } => Self::quote_floating(*premium_bps, *price_decimals, mid),
        }
    }

    fn quote_floating(premium_bps: i32, price_decimals: u8, mid: MidPrice) -> PriceQuote {
        let unpriceable = |reason| PriceQuote::Unpriceable {
            reason,
            premium_bps,
        };
        let (rate, mid_expires_at) = match mid {
            MidPrice::Available { rate, expires_at } => (rate, expires_at),
            MidPrice::NoOracleData => return unpriceable(UnpriceableReason::NoOracleData),
            MidPrice::StaleOracleData => return unpriceable(UnpriceableReason::StaleOracleData),
        };

        let multiplier = 1.0 + f64::from(premium_bps) / 10_000.0;
        match to_amount(rate * multiplier, price_decimals) {
            Some(price) => PriceQuote::Floating {
                price,
                mid_rate: rate,
                premium_bps,
                mid_expires_at,
            },
            None => unpriceable(UnpriceableReason::PriceOutOfRange),
        }
    }
}

/// A float price onto the fixed-point [`Amount`] everything else uses.
///
/// # Rounding is half-to-even, deliberately
///
/// The last minor unit has to go somewhere, and whoever it goes to gets it
/// on *every* trade — a merchant is a repeat player, so a systematic
/// half-cent is a real transfer even though any single instance is
/// negligible. Rounding toward the merchant is straightforwardly a small
/// theft from every taker. But rounding toward the taker is the same
/// systematic transfer pointed the other way, and merchants would simply
/// widen `premium_bps` to recover it — moving the cost back onto takers
/// where it is *less* visible than a premium they can read.
///
/// Half-to-even has no directional drift for either side, and its worst
/// case is half a minor unit rather than a full one. It also does not
/// depend on the advertisement's `Direction`, so the price shown to a
/// taker and the price quoted to the merchant are the same number — with
/// direction-dependent rounding, "the price" would not be a property of
/// the advertisement alone.
///
/// Returns `None` rather than saturating for anything an `Amount` cannot
/// represent; see [`UnpriceableReason::PriceOutOfRange`].
fn to_amount(value: f64, decimals: u8) -> Option<Amount> {
    let scaled = value * 10f64.powi(i32::from(decimals));
    if !scaled.is_finite() || scaled < 0.0 {
        return None;
    }
    let rounded = scaled.round_ties_even();
    // `u64::MAX` is not exactly representable as an `f64`, so comparing
    // against it directly would let values just under it through and then
    // wrap on the cast. `2^64` is exact, and `<` against it is not.
    if rounded >= 18_446_744_073_709_551_616.0 {
        return None;
    }
    Some(Amount::new(rounded as u64, decimals))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floating(premium_bps: i32) -> PricingModel {
        PricingModel::Floating {
            oracle_provider: "any".to_string(),
            premium_bps,
            price_decimals: 2,
        }
    }

    fn available(rate: f64) -> MidPrice {
        MidPrice::Available {
            rate,
            expires_at: Timestamp::from_millis(9_000),
        }
    }

    /// The headline case: 129.52 KES mid, +1.5%, quoted to the cent.
    #[test]
    fn a_floating_price_is_the_mid_plus_its_premium() {
        let quote = floating(150).quote(available(129.52));
        assert_eq!(
            quote,
            PriceQuote::Floating {
                // 129.52 * 1.015 = 131.4628, to the cent.
                price: Amount::new(13_146, 2),
                mid_rate: 129.52,
                premium_bps: 150,
                mid_expires_at: Timestamp::from_millis(9_000),
            }
        );
        assert_eq!(quote.price().unwrap().to_string(), "131.46");
    }

    /// `premium_bps` is signed for this: a merchant competing for flow may
    /// quote below mid, and that must not underflow into an enormous
    /// number.
    #[test]
    fn a_negative_premium_quotes_below_mid() {
        let quote = floating(-250).quote(available(129.52));
        assert_eq!(quote.price(), Some(Amount::new(12_628, 2)));
    }

    /// The failure this whole path exists to prevent: an advertisement
    /// whose oracle feed has lapsed must not sell at yesterday's rate. It
    /// has no price at all.
    #[test]
    fn a_stale_oracle_read_leaves_the_advertisement_unpriceable() {
        let quote = floating(150).quote(MidPrice::StaleOracleData);
        assert_eq!(
            quote,
            PriceQuote::Unpriceable {
                reason: UnpriceableReason::StaleOracleData,
                premium_bps: 150,
            }
        );
        assert_eq!(quote.price(), None, "a lapsed feed must not yield a number");
    }

    /// The other half: a pair nobody publishes is unpriceable rather than
    /// defaulting to the mid, to zero, or to the premium alone.
    #[test]
    fn a_pair_with_no_oracle_data_is_unpriceable_rather_than_invented() {
        let quote = floating(150).quote(MidPrice::NoOracleData);
        assert_eq!(
            quote,
            PriceQuote::Unpriceable {
                reason: UnpriceableReason::NoOracleData,
                premium_bps: 150,
            }
        );
        assert_eq!(quote.price(), None);
    }

    /// A premium below `-10_000` bps is a negative price. Refused, not
    /// clamped to zero — a free trade is not what the merchant configured.
    #[test]
    fn a_premium_below_negative_one_hundred_percent_is_out_of_range() {
        let quote = floating(-10_001).quote(available(129.52));
        assert_eq!(
            quote,
            PriceQuote::Unpriceable {
                reason: UnpriceableReason::PriceOutOfRange,
                premium_bps: -10_001,
            }
        );
    }

    #[test]
    fn a_premium_of_exactly_negative_one_hundred_percent_is_zero_not_an_error() {
        assert_eq!(
            floating(-10_000).quote(available(129.52)).price(),
            Some(Amount::new(0, 2))
        );
    }

    /// Half-to-even, both directions, so a change to plain `round()` (which
    /// rounds half away from zero, i.e. toward the merchant on a Sell)
    /// fails here.
    #[test]
    fn a_price_on_an_exact_half_rounds_to_even() {
        let quote = |rate| {
            PricingModel::Floating {
                oracle_provider: "any".to_string(),
                premium_bps: 0,
                price_decimals: 1,
            }
            .quote(available(rate))
        };
        // 0.25 -> 0.2 (down to even), 0.35 -> 0.4 (up to even).
        assert_eq!(quote(0.25).price(), Some(Amount::new(2, 1)));
        assert_eq!(quote(0.35).price(), Some(Amount::new(4, 1)));
    }

    /// A fixed advertisement resolves without consulting the oracle at
    /// all, so a dead feed never makes a fixed-price ad unpriceable.
    #[test]
    fn a_fixed_price_resolves_even_with_no_oracle_data() {
        let pricing = PricingModel::Fixed {
            price: Amount::new(12_840, 2),
        };
        let quote = pricing.quote(MidPrice::NoOracleData);
        assert_eq!(
            quote,
            PriceQuote::Fixed {
                price: Amount::new(12_840, 2)
            }
        );
        assert!(!quote.is_floating());
    }

    #[test]
    fn a_price_too_large_for_an_amount_is_out_of_range_rather_than_wrapping() {
        let pricing = PricingModel::Floating {
            oracle_provider: "any".to_string(),
            premium_bps: i32::MAX,
            price_decimals: 18,
        };
        assert_eq!(
            pricing.quote(available(1e30)),
            PriceQuote::Unpriceable {
                reason: UnpriceableReason::PriceOutOfRange,
                premium_bps: i32::MAX,
            }
        );
    }

    /// Two nodes handed the same read must produce the same number — the
    /// property that makes a quote safe to pin into a commitment.
    #[test]
    fn resolution_is_deterministic_for_a_given_read() {
        let mid = available(129.523_456_7);
        assert_eq!(floating(137).quote(mid), floating(137).quote(mid));
    }
}
