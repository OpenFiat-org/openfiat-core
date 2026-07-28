//! `openfiat-metrics` — Prometheus metrics and telemetry instrumentation.
//!
//! `MetricsRegistry` holds atomic `Counter`/`Gauge` handles (real
//! `Arc<Atomic*>`, safe to share across axum's worker threads — unlike
//! this workspace's `Rc`-based domain registries) and renders them in
//! Prometheus's text exposition format for a `/metrics` endpoint.

pub mod counter;
pub mod gauge;
pub mod registry;

pub use counter::Counter;
pub use gauge::Gauge;
pub use registry::MetricsRegistry;

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_version() {
        assert!(!version().is_empty());
    }
}
