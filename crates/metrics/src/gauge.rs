//! A value that can move in either direction — Prometheus's `gauge` type
//! (connected peer count, replicated record counts, ...).

use std::sync::atomic::{AtomicI64, Ordering};

#[derive(Debug, Default)]
pub struct Gauge(AtomicI64);

impl Gauge {
    pub fn set(&self, value: i64) {
        self.0.store(value, Ordering::Relaxed);
    }

    pub fn add(&self, delta: i64) {
        self.0.fetch_add(delta, Ordering::Relaxed);
    }

    pub fn sub(&self, delta: i64) {
        self.0.fetch_sub(delta, Ordering::Relaxed);
    }

    pub fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_in_either_direction() {
        let gauge = Gauge::default();
        gauge.set(10);
        gauge.add(5);
        gauge.sub(3);
        assert_eq!(gauge.get(), 12);
    }
}
