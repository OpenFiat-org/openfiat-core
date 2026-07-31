//! Wire-level constants: gossip event names (OFS-8100's RSV namespace)
//! and the timeout defaults from §12a's matrix.

use std::time::Duration;

pub const OFS_SPEC: u16 = 2200;

pub const EVENT_REQUESTED: &str = "ReservationRequested";
pub const EVENT_CANCELLED: &str = "ReservationCancelled";

/// §12a: Requested → Escrow Locked default window.
pub const VALIDATION_WINDOW: Duration = Duration::from_secs(30 * 60);

/// How often a node re-runs [`crate::ReservationRegistry::expire_stale`]
/// against [`VALIDATION_WINDOW`].
///
/// Declared here, next to the window it enforces, rather than beside the
/// timer that drives it: the two numbers are only meaningful as a pair,
/// and the sweep interval is not a scheduling preference — it is the
/// error bar on the promise `VALIDATION_WINDOW` makes.
///
/// A minute against thirty. The same reasoning `REGISTRY_SWEEP_INTERVAL`
/// records in `openfiat-rpc`: a sweep coarse relative to the threshold it
/// enforces turns "expires after thirty minutes" into "expires after
/// thirty to ninety", and the merchant waiting for their liquidity back
/// experiences the *upper* bound. Thirty sweeps fit inside the window,
/// comfortably clearing the ratio-of-five bar the registry sweep is held
/// to.
///
/// A minute rather than five for a reason specific to *this* sweep, and
/// the reason is convergence, not patience. Expiry is computed locally
/// (see [`crate::ReservationRegistry::expire_stale`]), so between the
/// instant a reservation's deadline passes and the instant each node's
/// timer next fires, nodes disagree about how much liquidity the
/// advertisement has. A taker whose request is accepted by an
/// already-swept node and re-applied on a not-yet-swept one is rejected
/// there for `InsufficientLiquidity`, and that node then holds no record
/// of a reservation the rest of the network has. That divergence window
/// *is* this constant, so it is chosen to be short, not merely tidy.
///
/// The cost is a full scan of the `reservations` column family every
/// minute — a family the node deliberately never prunes — so this trades
/// a scan that grows with lifetime trade volume against a divergence
/// window that does not. If that scan ever becomes the expensive thing,
/// the fix is an index of the still-locked reservations, not a slower
/// sweep.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// How far ahead of a node's own clock a requester's claimed timestamp
/// may be and still be accepted.
///
/// A reservation's deadline is derived from a timestamp the requester
/// signs, and a signature proves authorship, not honesty. Without a
/// bound, a requester dates their request a decade out and the
/// reservation never expires — [`VALIDATION_WINDOW`] becomes whatever
/// the taker feels like, and the sweep that returns a merchant's
/// liquidity never fires for it. The merchant's capital is held
/// hostage by a number the person holding it chose.
///
/// Only the future side is bounded. A timestamp in the past shortens the
/// requester's own window and expires their own reservation early, which
/// is not an attack on anyone else, and gossip delay makes a slightly
/// stale timestamp the normal case rather than a suspicious one.
///
/// Five minutes because it must clear real clock disagreement between
/// honest, unsynchronised machines without being a meaningful extension
/// of a thirty-minute window. This does mean two nodes can briefly
/// disagree about a request signed right at the boundary — but expiry is
/// already computed against local clocks (see [`SWEEP_INTERVAL`], which
/// sizes exactly that divergence deliberately), so this adds no new kind
/// of disagreement, only the same one at a bounded edge.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar `openfiat-rpc` already holds its registry sweep to, applied
    /// here so that raising [`SWEEP_INTERVAL`] or lowering
    /// [`VALIDATION_WINDOW`] cannot silently turn the window into a
    /// suggestion. Five sweeps inside the window is the minimum for the
    /// overshoot to be a rounding error rather than a second deadline.
    #[test]
    fn at_least_five_sweeps_fit_inside_the_validation_window() {
        assert!(
            SWEEP_INTERVAL * 5 <= VALIDATION_WINDOW,
            "a sweep every {SWEEP_INTERVAL:?} against a {VALIDATION_WINDOW:?} window lets a \
             reservation outlive its deadline by a visible fraction of the deadline itself"
        );
    }
}
