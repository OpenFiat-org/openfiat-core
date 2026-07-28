//! `openfiat-serialization` — Canonical serialization/deserialization for wire and storage formats.
//!
//! Two boundaries, two formats (decision log item 3 of the P2P networking
//! plan): [`wire`] (`postcard`) for internal Rust↔Rust messages inside the
//! gossip envelope and RocksDB values, and [`json`] (`serde_json`) for the
//! HTTP/RPC boundary where cross-language and human readability matter
//! more than size.

pub mod json;
pub mod wire;

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
