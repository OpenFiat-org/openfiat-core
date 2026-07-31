//! `openfiat-content` — attachments and avatars addressed by IPFS CID.
//!
//! A protocol record never carries a file. It carries a [`Cid`]: a
//! self-describing hash of content stored somewhere else, which any
//! consumer can verify the bytes against once fetched. That split is what
//! keeps a 10 MB receipt out of a gossip payload every node on the network
//! must store and replay, while still letting an arbitrator establish that
//! the image they are looking at is the one the party signed.
//!
//! Three things this crate deliberately does not do:
//!
//! - **It does not upload.** Pinning is an account with a provider, and a
//!   provider credential must never travel to a browser or into a signed
//!   record. Uploading belongs to whatever holds the credential — the
//!   interface's own server side — and this crate only ever sees the CID
//!   that came back.
//! - **It does not encrypt.** Attachments are public, verifiably so; see
//!   [`record`] for why that is the right answer for evidence rather than
//!   a gap to close later.
//! - **It does not fetch on sight.** A node that stored the bytes behind
//!   every CID it saw would be storing whatever anyone chose to point it
//!   at. What it does fetch and hold is the content referenced by
//!   *accepted* attachment records inside its own retention window — a set
//!   bounded by real trading volume, since an attachment needs a
//!   settlement and a settlement needs real escrow. Everything retrieved
//!   is checked with [`Cid::matches`] before it is kept, whether it came
//!   from a peer or a gateway.
//!
//! # Serving, which is a node's job and not a browser's
//!
//! [`bitswap`] lets a node answer the IPFS network for the content it
//! holds, over the node's own libp2p identity. That replaced an earlier
//! arrangement where each node ran a separate Kubo daemon — a second
//! identity, a second runtime, and an unauthenticated control port — and
//! it is what lets content serving be on by default rather than something
//! an operator had to install Go to opt into.
//!
//! Because bitswap's unit is the block, so is a node's. A file past
//! IPFS's chunk size arrives as a tree of blocks, each named by the hash
//! of its own bytes; [`dag`] walks it and [`held`] keeps what the walk
//! produced. What did not widen along with it is [`challenge`], which can
//! still only decide a CID whose digest covers a file — holding content
//! and proving by hash that you hold it are different claims, and running
//! them together would let a node pass a challenge it should fail.

pub mod bitswap;
pub mod challenge;
pub mod dag;
pub mod error;
pub mod events;
pub mod gateway;
pub mod held;
pub mod kubo;
pub mod pinning;
pub mod protocol;
pub mod record;
pub mod retention;
pub mod service;
pub mod store;

#[cfg(test)]
mod fixtures;

pub use challenge::{ChallengeOutcome, challengeable, judge};
pub use error::ContentError;
pub use events::SignedAttachmentPublish;
pub use gateway::{DEFAULT_GATEWAY, GatewayFetcher};
pub use held::{HeldContent, MAX_BLOCK_BYTES};
pub use kubo::KuboClient;
pub use openfiat_crypto::Cid;
pub use pinning::{PinError, PinningClient};
pub use record::{
    Attachment, AttachmentId, AttachmentSubject, MAX_ATTACHMENT_BYTES, MAX_CAPTION_CHARS, MediaType,
};
pub use retention::{MINIMUM_DAYS, Retention};
pub use service::AttachmentService;
pub use store::AttachmentRegistry;

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
