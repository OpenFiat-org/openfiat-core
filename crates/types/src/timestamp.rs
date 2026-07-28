//! Unix-epoch millisecond timestamps used throughout the protocol.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch (UTC).
///
/// A fixed-width integer rather than `SystemTime` so it serializes
/// identically across every language an SDK might be written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct Timestamp(u64);

impl Timestamp {
    /// The current wall-clock time.
    ///
    /// # Panics
    /// Panics if the system clock is set before the Unix epoch.
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_millis() as u64;
        Self(millis)
    }

    /// Construct from a raw millisecond count (e.g. one read back from storage).
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    /// The raw millisecond count.
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Milliseconds elapsed since `earlier`, or `None` if `earlier` is later than `self`.
    pub fn since(self, earlier: Timestamp) -> Option<u64> {
        self.0.checked_sub(earlier.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_after_the_epoch() {
        assert!(Timestamp::now().as_millis() > 0);
    }

    #[test]
    fn since_computes_elapsed_and_rejects_negative() {
        let earlier = Timestamp::from_millis(1_000);
        let later = Timestamp::from_millis(1_500);
        assert_eq!(later.since(earlier), Some(500));
        assert_eq!(earlier.since(later), None);
    }
}
