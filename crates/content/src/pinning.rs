//! Holding content, for the node operators who opt in.
//!
//! # Opt-in, and why that is not timidity
//!
//! A node that automatically fetched every CID it saw would store
//! whatever anyone chose to point it at. Attachments are bounded at 10 MB
//! each, but nothing bounds how many a stranger publishes, so automatic
//! pinning would hand every node on the network an unbounded, remotely
//! controlled disk bill.
//!
//! Two things keep the opted-in case sane. Pinning requires
//! `--ipfs-api-url`, so an operator who has not chosen to run an IPFS
//! daemon stores nothing at all. And the set worth pinning is the content
//! referenced by *accepted* attachment records, each of which names a
//! settlement — and a settlement cannot be conjured, it needs a real
//! reservation against real escrow. The bound on what a node can be asked
//! to store is therefore the network's real trading volume rather than
//! anyone's willingness to spam.
//!
//! # What this is for
//!
//! Answering a retrievability challenge (see [`crate::challenge`]) and
//! keeping evidence alive for a dispute that may open weeks after the
//! trade. It is not a CDN and not a backup service: an operator who stops
//! pinning breaks nothing, they merely stop earning the premium and stop
//! being one of the places the content can be fetched from.

use openfiat_crypto::Cid;

/// The largest response this node will buffer from its own IPFS daemon.
///
/// Matches [`crate::MAX_ATTACHMENT_BYTES`], and is enforced by streaming
/// rather than by trusting a `Content-Length`: a CID names content of
/// unknown size until it arrives, and a header is a claim by whatever is
/// serving it. Without a streaming cap, one CID naming a large file is
/// enough to exhaust a node's memory.
pub const MAX_FETCH_BYTES: usize = crate::MAX_ATTACHMENT_BYTES as usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinError {
    /// The daemon refused, or could not be reached.
    Unavailable(String),
    /// The content exceeded [`MAX_FETCH_BYTES`].
    TooLarge,
    /// The bytes retrieved were not the ones the CID names. A local
    /// daemon should never do this; that it is checked anyway is the
    /// point of a content address.
    ContentMismatch,
}

impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(detail) => write!(f, "IPFS daemon unavailable: {detail}"),
            Self::TooLarge => write!(f, "content exceeds {MAX_FETCH_BYTES} bytes"),
            Self::ContentMismatch => write!(f, "retrieved bytes do not match the CID"),
        }
    }
}

impl std::error::Error for PinError {}

/// A node's connection to whatever actually stores bytes.
///
/// A trait rather than a concrete client so the node's own logic can be
/// tested without an IPFS daemon, and so an operator can put something
/// other than Kubo behind it.
#[async_trait::async_trait(?Send)]
pub trait PinningClient {
    /// Asks the daemon to hold `cid` durably.
    async fn pin(&self, cid: &Cid) -> Result<(), PinError>;

    /// Retrieves `cid`'s content, capped at [`MAX_FETCH_BYTES`].
    ///
    /// Implementations must verify what they retrieved against the CID
    /// before returning it. A node serves this to challengers, and
    /// serving unverified bytes would turn one bad local fetch into a
    /// failed challenge that looks like dishonesty.
    async fn fetch(&self, cid: &Cid) -> Result<Vec<u8>, PinError>;
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// An in-memory stand-in, for testing the logic around pinning
    /// without a daemon.
    #[derive(Default)]
    pub struct MemoryPinningClient {
        stored: RefCell<HashMap<String, Vec<u8>>>,
        /// When set, every call fails — the "operator's daemon is down"
        /// case, which must not be indistinguishable from "node is lying".
        pub offline: bool,
    }

    impl MemoryPinningClient {
        pub fn with_content(entries: &[(&Cid, &[u8])]) -> Self {
            let stored = entries
                .iter()
                .map(|(cid, bytes)| (cid.as_str().to_string(), bytes.to_vec()))
                .collect();
            Self {
                stored: RefCell::new(stored),
                offline: false,
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PinningClient for MemoryPinningClient {
        async fn pin(&self, cid: &Cid) -> Result<(), PinError> {
            if self.offline {
                return Err(PinError::Unavailable("offline".into()));
            }
            self.stored
                .borrow_mut()
                .entry(cid.as_str().to_string())
                .or_default();
            Ok(())
        }

        async fn fetch(&self, cid: &Cid) -> Result<Vec<u8>, PinError> {
            if self.offline {
                return Err(PinError::Unavailable("offline".into()));
            }
            let stored = self.stored.borrow();
            let bytes = stored
                .get(cid.as_str())
                .ok_or_else(|| PinError::Unavailable("not pinned here".into()))?;
            if !cid.matches(bytes) {
                return Err(PinError::ContentMismatch);
            }
            Ok(bytes.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::MemoryPinningClient;
    use super::*;
    use crate::fixtures;

    const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

    #[tokio::test]
    async fn serves_content_it_holds() {
        let cid = fixtures::probe_cid();
        let client = MemoryPinningClient::with_content(&[(&cid, PROBE_CONTENT)]);
        assert_eq!(client.fetch(&cid).await, Ok(PROBE_CONTENT.to_vec()));
    }

    #[tokio::test]
    async fn content_it_does_not_hold_is_unavailable_not_a_mismatch() {
        // The two must stay distinguishable: one means "ask someone
        // else", the other means something is seriously wrong.
        let client = MemoryPinningClient::default();
        assert!(matches!(
            client.fetch(&fixtures::probe_cid()).await,
            Err(PinError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn bytes_that_do_not_match_the_cid_are_refused_even_from_our_own_daemon() {
        let cid = fixtures::probe_cid();
        let client = MemoryPinningClient::with_content(&[(&cid, b"tampered")]);
        assert_eq!(client.fetch(&cid).await, Err(PinError::ContentMismatch));
    }

    #[tokio::test]
    async fn an_offline_daemon_fails_loudly_rather_than_returning_nothing() {
        let mut client = MemoryPinningClient::default();
        client.offline = true;
        assert!(matches!(
            client.pin(&fixtures::probe_cid()).await,
            Err(PinError::Unavailable(_))
        ));
    }
}
