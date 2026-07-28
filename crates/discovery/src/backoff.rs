//! Reconnection backoff (OFS-1100 §15).
//!
//! "Unexpected disconnects SHOULD trigger exponential backoff... Randomized
//! jitter SHOULD be added to reduce synchronized reconnect storms." The
//! base sequence is a pure function (easy to test exactly); jitter is
//! applied separately so callers needing determinism can skip it.

use rand::{Rng, RngExt};
use std::time::Duration;

/// The base backoff sequence's ceiling — §15's example sequence caps at 60s.
const MAX_DELAY_SECS: u64 = 60;

/// The base (jitter-free) delay before reconnect attempt number `attempt`
/// (1-indexed): 1s, 2s, 4s, 8s, 16s, 30s, 60s, 60s, ... matching §15's
/// example exactly for the first six attempts, then holding at the ceiling.
pub fn base_delay(attempt: u32) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }
    // 1, 2, 4, 8, 16 for attempts 1-5; §15's own sequence breaks the
    // doubling pattern at attempt 6 (30s, not 32s) before holding at 60s.
    let secs = match attempt {
        1..=5 => 1u64 << (attempt - 1),
        6 => 30,
        _ => MAX_DELAY_SECS,
    };
    Duration::from_secs(secs)
}

/// `base_delay(attempt)` plus up to 20% random jitter, so many nodes
/// reconnecting to the same peer at once don't all retry in lockstep.
pub fn jittered_delay(attempt: u32, rng: &mut impl Rng) -> Duration {
    let base = base_delay(attempt);
    let jitter_ms = rng.random_range(0..=(base.as_millis() as u64 / 5).max(1));
    base + Duration::from_millis(jitter_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_specs_example_sequence() {
        let expected_secs = [1, 2, 4, 8, 16, 30, 60, 60];
        for (i, &secs) in expected_secs.iter().enumerate() {
            let attempt = (i + 1) as u32;
            assert_eq!(base_delay(attempt), Duration::from_secs(secs), "attempt {attempt}");
        }
    }

    #[test]
    fn jitter_never_reduces_the_delay_below_the_base() {
        let mut rng = rand::rng();
        for attempt in 1..=8 {
            let jittered = jittered_delay(attempt, &mut rng);
            assert!(jittered >= base_delay(attempt));
        }
    }
}
