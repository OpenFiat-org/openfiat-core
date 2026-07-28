//! `openfiat-api` — OpenRPC specification + interactive reference
//! documentation for `openfiat-rpc`.
//!
//! `GET /openrpc.json` serves an OpenRPC 1.2.6 document (the JSON-RPC
//! analog of an OpenAPI/Swagger spec) generated from the exact dispatch
//! table `openfiat-rpc` runs, so the method list can never silently
//! drift out of sync with the code — see the `openrpc` module doc for
//! what is and isn't automatically derived. `GET /docs` serves a
//! self-contained, swagger-like interactive reference page: browse
//! every method's params/result shape and run it live against `/rpc` on
//! the same origin.

pub mod openrpc;
pub mod server;

pub use server::router;

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
