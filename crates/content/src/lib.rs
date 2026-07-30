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
//! - **It does not fetch.** A node that stored the referenced bytes would
//!   be storing whatever anyone chose to point it at. Retrieval is the
//!   viewer's, against a gateway, checked with [`Cid::matches`].

pub mod challenge;
pub mod error;
pub mod events;
pub mod held;
pub mod kubo;
pub mod pinning;
pub mod protocol;
pub mod record;
pub mod service;
pub mod store;

#[cfg(test)]
mod fixtures;

pub use challenge::{ChallengeOutcome, challengeable, judge};
pub use error::ContentError;
pub use events::SignedAttachmentPublish;
pub use held::{HeldContent, MAX_HELD_BYTES};
pub use kubo::KuboClient;
pub use openfiat_crypto::Cid;
pub use pinning::{PinError, PinningClient};
pub use record::{
    Attachment, AttachmentId, AttachmentSubject, MAX_ATTACHMENT_BYTES, MAX_CAPTION_CHARS, MediaType,
};
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
