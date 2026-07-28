//! Wire-level constants: gossip event names (OFS-8100's RSV namespace)
//! and the timeout defaults from §12a's matrix.

use std::time::Duration;

pub const OFS_SPEC: u16 = 2200;

pub const EVENT_REQUESTED: &str = "ReservationRequested";
pub const EVENT_CANCELLED: &str = "ReservationCancelled";

/// §12a: Requested → Escrow Locked default window.
pub const VALIDATION_WINDOW: Duration = Duration::from_secs(30 * 60);
