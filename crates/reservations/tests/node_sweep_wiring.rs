//! Proves the expiry sweep is actually *reached* by the running node.
//!
//! Every unit test in this crate exercises
//! `ReservationRegistry::expire_stale` by calling it. That is worth
//! nothing on its own: this workspace has now shipped five separate
//! pieces of correct, unit-tested maintenance code that no code path in
//! the node ever called — the `peers` column family, `PeerCache::
//! expire_stale`, `LivenessLedger::prune_through`, the snapshot
//! announcement index, and this sweep. All five passed their tests. The
//! defect was never in the function; it was in the absence of a caller,
//! and no test that calls the function itself can see that.
//!
//! So this test asserts on the caller. The node's event loop lives in
//! `openfiat-rpc`, which depends on this crate — a Cargo dev-dependency
//! back the other way would be a cycle — so the loop is read as source
//! text at compile time instead. `include_str!` across crate boundaries
//! is already how `openfiat-chain` pins itself to the on-chain program
//! constants, and it gives the same property here: change the actor and
//! this test recompiles against the change.
//!
//! Text matching is a blunt instrument and this is deliberately the only
//! place it is used. It is blunt in the safe direction — it can complain
//! about a wiring that exists under another name, which is loud and takes
//! a minute to fix, but it cannot stay quiet about a wiring that is gone,
//! which is the failure that cost five bugs.

/// The node's event loop, as text. Read from source rather than reached
/// through a dependency, for the cycle reason above.
const ACTOR_SOURCE: &str = include_str!("../../rpc/src/actor.rs");

/// Everything before `#[cfg(test)]`, with whitespace removed.
///
/// Cutting the test module off matters: a sweep called only from
/// `actor.rs`'s own unit tests is exactly the bug, dressed as the fix.
/// Removing whitespace makes the search indifferent to how rustfmt
/// chooses to break `state.reservations.expire_stale(...)` across lines.
fn shipped_loop() -> String {
    let source = match ACTOR_SOURCE.find("#[cfg(test)]") {
        Some(at) => &ACTOR_SOURCE[..at],
        None => ACTOR_SOURCE,
    };
    source.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Without this call, a reservation stuck in `EscrowLocked` past its
/// validation window never expires, and the advertisement liquidity it
/// reserved is never returned. The merchant keeps a permanently reduced
/// balance on their own inventory and nothing anywhere says why.
#[test]
fn the_node_event_loop_calls_the_reservation_expiry_sweep() {
    assert!(
        shipped_loop().contains("reservations.expire_stale("),
        "openfiat-rpc's event loop no longer calls the reservation expiry sweep. Reservations \
         will sit in EscrowLocked forever and the liquidity they hold will never be released \
         back to the advertisement. Restore the `reservation_sweep.tick()` arm in \
         crates/rpc/src/actor.rs. If you are seeing this before that arm has landed for the \
         first time, this failure is the bug being reported, not a broken test — the fix is \
         the arm, never deleting this."
    );
}

/// The cadence has to come from `protocol::SWEEP_INTERVAL`, not from a
/// duration typed at the call site, because the interval is only
/// meaningful against `VALIDATION_WINDOW` — and that pairing is asserted
/// in `protocol`, where both live. A literal at the call site would drift
/// away from the window with nothing to notice.
#[test]
fn the_sweep_cadence_comes_from_the_constant_that_is_checked_against_the_window() {
    assert!(
        shipped_loop().contains("protocol::SWEEP_INTERVAL"),
        "the reservation sweep timer in crates/rpc/src/actor.rs must be built from \
         openfiat_reservations::protocol::SWEEP_INTERVAL. That constant is the one checked \
         against VALIDATION_WINDOW; a duration written at the call site is not. Like the test \
         above, this failing before the arm has landed is the report, not a broken test."
    );
}
