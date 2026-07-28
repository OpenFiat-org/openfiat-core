//! `openfiat-sessions` — Encrypted session establishment and lifecycle between peers.
//!
//! Implements OFS-1400 on top of `openfiat_gossip`: session establishment,
//! renewal, revocation, and migration travel as signed gossip events, and
//! every node derives its local session index purely by consuming them —
//! the same replication pattern used throughout this workspace. Real OFS-
//! 5100 wallet-authentication challenge/response (referenced but never
//! published as a spec) isn't modeled separately here: `SessionCreate`'s
//! own signature over the session record already proves wallet control,
//! the same self-consistency check every other CREATE-type event in this
//! workspace uses.

pub mod error;
pub mod events;
pub mod protocol;
pub mod record;
pub mod service;
pub mod store;

pub use error::SessionError;
pub use record::{Session, SessionId};
pub use service::SessionService;
pub use store::SessionRegistry;

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
