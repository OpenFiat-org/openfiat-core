//! A monotonically increasing value — Prometheus's `counter` type.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    pub fn inc(&self) {
        self.add(1);
    }

    pub fn add(&self, delta: u64) {
        self.0.fetch_add(delta, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_zero_and_accumulates() {
        let counter = Counter::default();
        assert_eq!(counter.get(), 0);
        counter.inc();
        counter.add(4);
        assert_eq!(counter.get(), 5);
    }
}
