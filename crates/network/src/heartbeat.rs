//! Heartbeat timing (OFNP §18), using the defaults proposed in
//! `docs/architecture.md` (`[PROPOSED — NEEDS SIGN-OFF]`).
//!
//! Actual liveness checking is delegated to libp2p's `ping` behaviour in
//! [`crate::behaviour`] rather than a hand-rolled heartbeat protocol —
//! `ping` already provides periodic round-trip probing and a configurable
//! failure timeout, which is exactly what §18 asks for.

use std::time::Duration;

/// How often a peer is pinged to verify session liveness.
pub const INTERVAL: Duration = Duration::from_secs(15);

/// A session is considered dead after this long without a successful
/// heartbeat (three missed intervals).
pub const TIMEOUT: Duration = Duration::from_secs(45);
