//! `openfiat-rpc` — JSON-RPC / WebSocket server surface.
//!
//! One POST endpoint speaking JSON-RPC 2.0 with Solana-style `getX`/
//! `sendX` camelCase method names (see the `jsonrpc` module doc), backed
//! by real (not mocked) in-process state — every domain crate this
//! workspace has built, composed in `state::NodeState`. See `actor` for
//! why that state lives behind a channel instead of axum's shared
//! `State` extractor.

pub mod actor;
pub mod dispatch;
pub mod error;
pub mod jsonrpc;
pub mod methods;
pub mod server;
pub mod state;

pub use actor::{RpcHandle, spawn_actor};
pub use dispatch::MethodTable;
pub use error::RpcError;
pub use server::router;
pub use state::NodeState;

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
