//! Per-session sequence number tracking (OFNP §15).
//!
//! "Every session maintains monotonically increasing sequence numbers...
//! Duplicate sequence numbers SHALL be discarded." A single "highest
//! accepted so far" counter is sufficient to enforce that: anything at or
//! below it is a duplicate or a replay, anything above it is accepted and
//! becomes the new high-water mark.

use crate::error::NetworkError;

/// Tracks the highest sequence number accepted on one session, rejecting
/// duplicates and out-of-order (non-increasing) numbers.
#[derive(Debug, Default)]
pub struct SequenceTracker {
    highest_accepted: Option<u64>,
}

impl SequenceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept `sequence` if it's strictly greater than every sequence
    /// number accepted so far on this session; otherwise reject it.
    pub fn accept(&mut self, sequence: u64) -> Result<(), NetworkError> {
        match self.highest_accepted {
            Some(highest) if sequence == highest => Err(NetworkError::DuplicateSequence),
            Some(highest) if sequence < highest => Err(NetworkError::OutOfOrderSequence),
            _ => {
                self.highest_accepted = Some(sequence);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_strictly_increasing_sequence_numbers() {
        let mut tracker = SequenceTracker::new();
        assert!(tracker.accept(1).is_ok());
        assert!(tracker.accept(2).is_ok());
        assert!(tracker.accept(10).is_ok());
    }

    #[test]
    fn rejects_a_repeated_sequence_number() {
        let mut tracker = SequenceTracker::new();
        tracker.accept(5).unwrap();
        let err = tracker.accept(5).unwrap_err();
        assert!(matches!(err, NetworkError::DuplicateSequence));
    }

    #[test]
    fn rejects_a_sequence_number_below_the_high_water_mark() {
        let mut tracker = SequenceTracker::new();
        tracker.accept(5).unwrap();
        let err = tracker.accept(3).unwrap_err();
        assert!(matches!(err, NetworkError::OutOfOrderSequence));
    }
}
