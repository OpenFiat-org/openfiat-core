//! `openfiat-reputation` — Behavioral reputation scoring engine.
//!
//! Implements OFS-3000 (ORE) as a pure read-side aggregate over
//! `openfiat-reservations`, `openfiat-settlement`, and `openfiat-disputes`
//! — see the `view` module doc for why this crate has no signed events or
//! store of its own.

pub mod record;
pub mod view;

pub use record::{MerchantTier, ReputationProfile};
pub use view::ReputationView;

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
