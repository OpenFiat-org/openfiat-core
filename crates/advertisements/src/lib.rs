//! `openfiat-advertisements` — Advertisement publication and matching.
//!
//! Related specification: OFS-2100 (OAP).
//! This crate currently defines architecture only: module layout and public
//! surface will be filled in during implementation. No business logic lives
//! here yet.

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
