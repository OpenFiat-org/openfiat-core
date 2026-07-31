//! Who a node with no history of its own is willing to take a worldview
//! from.
//!
//! # The problem these anchors exist for
//!
//! `SnapshotStore::import` verifies a great deal: the announcement was
//! signed, the producer is a registered snapshot provider, the bytes are
//! the announced size, and they hash to the announced `state_root`. Every
//! one of those checks passes on a **forged** snapshot, because they
//! establish that the bytes are what the announcer said they were — not
//! that what the announcer said is true. Nothing in the protocol
//! establishes what the correct state root at a given height *is*.
//!
//! For a node that already has a checkpoint that gap is narrow: it has its
//! own history to judge against, and a snapshot older than its checkpoint
//! is refused outright. For a node bootstrapping from nothing it is total.
//! Such a node has no basis whatsoever for preferring an honest producer's
//! snapshot over a self-consistent fabrication at height 10^9, and a
//! snapshot is the node's *entire worldview* — so getting this wrong is
//! not a degraded start, it is a node that believes a fiction and then
//! serves it onward.
//!
//! # What this is, honestly
//!
//! Weak subjectivity, the same shape a syncing Ethereum client uses. A
//! node with no checkpoint accepts its first snapshot only from a pinned
//! key. After that it has history, and the stake requirement on
//! registration governs instead.
//!
//! It is a trust assumption and should be described as one rather than
//! dressed up as trustlessness. What makes it acceptable is that it
//! applies **once**, to a node that has nothing to lose, and that the
//! alternative is not "trustless bootstrap" but "believe whoever answers
//! first".
//!
//! # Why compile-time, and why additive-only
//!
//! Pinned as constants for the reason `openfiat_chain::PROGRAM_IDS` is:
//! this is protocol identity, not deployment configuration. An operator
//! who could edit it could be persuaded to, and a node whose trust anchors
//! come from a config file is one bad systemd unit away from trusting an
//! attacker.
//!
//! `--trusted-snapshot-provider` therefore *adds* and can never remove. An
//! operator can trust their own infrastructure; nothing they can
//! misconfigure silently un-trusts the pinned set, which is precisely what
//! a tampered configuration would try first.

use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey};

/// AllenHark's snapshot trust anchors, base58 Ed25519 public keys.
///
/// Deliberately **not** the mint authority or the program upgrade
/// authority. Those must stay offline; these sign snapshot announcements
/// from a running node, so they are operational keys and are held
/// separately. A compromise here produces bad snapshots — recoverable by a
/// release that rotates these constants — rather than a minted supply or a
/// replaced program, which are not recoverable at all.
pub const TRUSTED_SNAPSHOT_PROVIDERS: [&str; 2] = [
    "ALLENLMtV1zEAHT3xpVryqcbdPCB8c9JhM1Jdbe5XHg5",
    "A11ENCKCBxZxEbXQmqs6mTmJkP8gjcA7xqfLD5BxfRpp",
];

/// The set of producers this node will take a *first* snapshot from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustAnchors {
    peers: Vec<PeerId>,
}

/// An operator-supplied anchor that is not a usable identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorError {
    pub supplied: String,
}

impl std::fmt::Display for AnchorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is not a base58 Ed25519 public key, so no node could ever match it",
            self.supplied
        )
    }
}

impl std::error::Error for AnchorError {}

impl TrustAnchors {
    /// The pinned set alone.
    pub fn pinned() -> Self {
        Self {
            peers: TRUSTED_SNAPSHOT_PROVIDERS
                .iter()
                .map(|key| {
                    parse(key).expect("a pinned trust anchor is a valid key, checked by test")
                })
                .collect(),
        }
    }

    /// The pinned set plus whatever the operator named.
    ///
    /// Union, never replacement — see this module's own note on why.
    /// A malformed entry is an error rather than a silent skip: an
    /// operator who mistypes their own anchor would otherwise get a node
    /// that trusts only the pinned keys and says nothing about it, which
    /// looks exactly like the configuration working.
    pub fn with_operator<S: AsRef<str>>(extra: &[S]) -> Result<Self, AnchorError> {
        let mut anchors = Self::pinned();
        for key in extra {
            let peer = parse(key.as_ref()).ok_or_else(|| AnchorError {
                supplied: key.as_ref().to_string(),
            })?;
            if !anchors.peers.contains(&peer) {
                anchors.peers.push(peer);
            }
        }
        Ok(anchors)
    }

    /// Whether a node with no checkpoint may adopt this producer's
    /// worldview.
    pub fn trusts(&self, producer: &PeerId) -> bool {
        self.peers.contains(producer)
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

impl Default for TrustAnchors {
    fn default() -> Self {
        Self::pinned()
    }
}

/// A base58 Ed25519 public key as the PeerId a signed announcement carries.
///
/// The constants are written as public keys because that is how a key is
/// exchanged and how it appears in a Solana-style address; a registration
/// is identified by the PeerId derived from it. Converting here rather
/// than storing PeerIds keeps the pinned constants in the form a human can
/// actually compare against a key they were given.
fn parse(base58_key: &str) -> Option<PeerId> {
    let bytes: [u8; 32] = bs58::decode(base58_key).into_vec().ok()?.try_into().ok()?;
    peer_id_from_public_key(&PublicKey::from_bytes(bytes)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_crypto::Keypair;

    #[test]
    fn every_pinned_anchor_is_a_usable_identity() {
        // `pinned()` panics on a malformed constant, and this is what
        // makes that `expect` safe rather than hopeful. A typo in one of
        // these would otherwise take down every node at startup — or,
        // worse under a silent-skip implementation, quietly leave the
        // network with one anchor instead of two and nothing to say so.
        for key in TRUSTED_SNAPSHOT_PROVIDERS {
            assert!(parse(key).is_some(), "{key} is not a usable anchor");
        }
        assert_eq!(TrustAnchors::pinned().len(), 2);
    }

    #[test]
    fn a_stranger_is_not_trusted_for_a_first_snapshot() {
        let stranger = Keypair::generate();
        let peer = peer_id_from_public_key(&stranger.public_key()).unwrap();
        assert!(!TrustAnchors::pinned().trusts(&peer));
    }

    #[test]
    fn an_operator_anchor_is_added_and_the_pinned_ones_survive() {
        // The whole point of additive-only. If this ever became a
        // replacement, an operator supplying one key would silently drop
        // both pinned anchors and this assertion is what notices.
        let operator = Keypair::generate();
        let operator_key = bs58::encode(operator.public_key().as_bytes()).into_string();
        let anchors = TrustAnchors::with_operator(&[operator_key]).unwrap();

        let operator_peer = peer_id_from_public_key(&operator.public_key()).unwrap();
        assert!(anchors.trusts(&operator_peer));
        assert_eq!(anchors.len(), 3);
        for key in TRUSTED_SNAPSHOT_PROVIDERS {
            assert!(anchors.trusts(&parse(key).unwrap()), "{key} was dropped");
        }
    }

    #[test]
    fn a_mistyped_operator_anchor_is_an_error_rather_than_a_silent_skip() {
        // A node that quietly ignored this would look exactly like one
        // where the flag worked, right up until the operator's own
        // provider was refused.
        let result = TrustAnchors::with_operator(&["not-a-key"]);
        assert_eq!(
            result,
            Err(AnchorError {
                supplied: "not-a-key".to_string()
            })
        );
    }

    #[test]
    fn naming_a_pinned_anchor_again_does_not_duplicate_it() {
        let anchors = TrustAnchors::with_operator(&[TRUSTED_SNAPSHOT_PROVIDERS[0]]).unwrap();
        assert_eq!(anchors.len(), 2);
    }
}
