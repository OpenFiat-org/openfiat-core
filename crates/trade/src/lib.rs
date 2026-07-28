//! `openfiat-trade` — Trade lifecycle state machine.
//!
//! Implements OFS-2000 (OFTP) §9: "Every trade is the composition of two
//! sub-protocols, each with its own canonical state machine... This
//! specification does not redefine either state machine." This crate is
//! therefore a read-time view over `openfiat-reservations` and
//! `openfiat-settlement`'s own replicated state, not a third independent
//! state machine — see the `view` module.

pub mod view;

pub use view::{Trade, TradeStatus, TradeView};

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
