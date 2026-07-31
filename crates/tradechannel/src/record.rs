//! The replicated shape of a trade channel: who may read it, and what
//! has been written into it.
//!
//! Everything in this module is gossiped to every node and kept forever.
//! That is the constraint the whole design answers to, so it is worth
//! being exact about what a node operator holding a replica actually
//! sees:
//!
//! - **Grants**: which peers can read a channel, who granted them, and
//!   when. The sealed key itself is opaque — it opens only under the
//!   recipient's private key.
//! - **Entries**: which settlement, which party wrote it, in what order,
//!   at what claimed time, whether it is payment details or chat, and how
//!   many bytes it padded to.
//! - **Neither** the payment details nor a single word of the
//!   conversation.
//!
//! The metadata in that first pair is real and unavoidable: a replicated
//! log cannot carry an entry without carrying the fact that it exists. It
//! is written down here rather than glossed over, because "sealed" must
//! mean a specific thing and not a comforting one.

use crate::key::{ChannelCiphertext, ChannelKeyId, EntryBinding};
use openfiat_crypto::SealedBox;
use openfiat_settlement::SettlementId;
use openfiat_types::{PeerId, Timestamp};

/// What an entry is, which decides how a client renders it and is bound
/// into the payload's associated data so the label cannot be changed
/// after the fact (see [`crate::key`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EntryKind {
    /// The instructions one party must follow to pay the other: a bank
    /// account, a mobile-money number, a reference. The thing this
    /// feature exists for.
    PaymentDetails,
    /// Free-text conversation between the two parties.
    Message,
}

impl EntryKind {
    /// The stable wire name, used as a domain tag inside the AEAD binding.
    /// Renaming a variant changes every binding derived from it, which is
    /// intentional: a label is only meaningful against a fixed vocabulary.
    pub const fn name(self) -> &'static str {
        match self {
            Self::PaymentDetails => "PaymentDetails",
            Self::Message => "Message",
        }
    }
}

/// Why a peer is allowed to read a channel.
///
/// Derived by the registry from state it can check for itself — the
/// settlement's parties, and the arbitrators who have joined a dispute
/// over it — never asserted by the granter. A granter who could name the
/// role could grant "Party" to anyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GrantRole {
    /// The buyer or the seller. A party granting to *themselves* is legal
    /// and expected: it is how a client that lost its local state
    /// recovers the channel key from the network on a new device.
    Party,
    /// An arbitrator who has joined a dispute over this settlement
    /// (OFS-2400). Only reachable after that join, which is the honest
    /// answer to "an arbitrator is not known when the messages are
    /// written".
    Arbitrator,
}

/// One peer's copy of the channel key, sealed to their identity key.
///
/// This is `openfiat_crypto::seal` used exactly as
/// `openfiat-notifications` uses it for delivery destinations — the same
/// mechanism, not a second one. What is sealed here is 32 bytes of key
/// rather than an email address, which is the only reason widening a
/// channel's readership costs one small event instead of a re-encryption
/// of its history.
///
/// # What a grant does not do
///
/// It cannot be revoked. The recipient already holds the sealed bytes,
/// which are replicated to every node forever; deleting the record would
/// remove the *evidence* of disclosure and none of the disclosure. A
/// party who wants a fresh audience without the old one starts a new
/// channel key, and every entry from that point carries the new
/// [`ChannelKeyId`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KeyGrant {
    pub settlement_id: SettlementId,
    pub granter: PeerId,
    pub recipient: PeerId,
    pub role: GrantRole,
    /// Which key this grant carries, so a client with several can tell
    /// which entries it opens without trial decryption.
    pub key_id: ChannelKeyId,
    /// The channel key, readable only by `recipient`.
    ///
    /// The node cannot check that this really contains the key it claims
    /// to — a sealed box is opaque to everyone but its recipient, which
    /// is the point. A granter can therefore seal garbage. That is
    /// self-defeating rather than dangerous: the only thing it achieves
    /// is that the recipient cannot read a channel the granter wanted
    /// them to read, and `key_id` makes even that detectable, since the
    /// recipient can hash what they opened and compare.
    pub sealed_key: SealedBox,
    pub granted_at: Timestamp,
}

/// One thing written into a channel.
///
/// Identified by `(settlement_id, author, sequence)` rather than by a
/// client-chosen id. That choice is doing real work: an id an author
/// picks freely can be *squatted* — a counterparty who guesses or
/// front-runs it writes first and the real entry is rejected as a
/// duplicate. Here the author is part of the identity and only the
/// author's own signature is accepted at their own sequence numbers, so
/// neither party can reach into the other's space at all. It also gives
/// each side a contiguous run of numbers, which is what lets a client
/// notice a *missing* message rather than silently rendering a
/// conversation with a hole in it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChannelEntry {
    pub settlement_id: SettlementId,
    pub author: PeerId,
    /// The author's own counter for this channel, from zero.
    pub sequence: u64,
    pub kind: EntryKind,
    pub payload: ChannelCiphertext,
    /// The author's claimed time, taken from their signed event.
    ///
    /// Claimed, not witnessed: nothing stops a client backdating its own
    /// message. It is what orders a rendered conversation because it is
    /// the only ordering both parties can agree on without a clock they
    /// share — and an arbitrator weighing a backdated message has the
    /// author's signature over that exact timestamp to weigh it with.
    pub posted_at: Timestamp,
}

impl ChannelEntry {
    /// The associated data this entry's payload was encrypted under —
    /// everything a reader needs to hand [`crate::key::open_entry`].
    pub fn binding(&self) -> EntryBinding<'_> {
        EntryBinding {
            settlement_id: &self.settlement_id,
            author: &self.author,
            sequence: self.sequence,
            kind: self.kind.name(),
        }
    }
}

/// A whole channel as one node holds it.
///
/// Assembled at read time from the two column families rather than stored
/// as a unit, so an entry and a grant that arrive out of order (which
/// gossip guarantees will happen) never need to be reconciled.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TradeChannel {
    pub settlement_id: SettlementId,
    /// Ordered by recipient then granter, so two nodes render the same
    /// list.
    pub grants: Vec<KeyGrant>,
    /// Ordered by claimed time, then by author and sequence so that two
    /// entries claiming the same instant still order identically
    /// everywhere.
    pub entries: Vec<ChannelEntry>,
}

impl TradeChannel {
    /// Every peer that can read this channel, deduplicated.
    ///
    /// Answerable by anyone holding a replica, and deliberately so: who
    /// can read a trade's conversation is exactly the kind of fact that
    /// should be auditable without reading it. It is also what makes a
    /// party's *refusal* to disclose visible — an arbitrator who was
    /// never granted the key can see that, and so can everyone else.
    pub fn readers(&self) -> Vec<PeerId> {
        let mut readers: Vec<PeerId> = self
            .grants
            .iter()
            .map(|grant| grant.recipient.clone())
            .collect();
        readers.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        readers.dedup();
        readers
    }

    /// Whether `peer` holds a grant on this channel. The read gate at the
    /// RPC boundary uses this to let a granted arbitrator fetch a channel
    /// they are not a party to.
    pub fn is_reader(&self, peer: &PeerId) -> bool {
        self.grants.iter().any(|grant| &grant.recipient == peer)
    }

    /// Every grant addressed to `peer`, newest first.
    ///
    /// Plural because both parties may grant the same arbitrator, and a
    /// recipient should try each: a single party sealing a wrong key
    /// would otherwise be able to lock an arbitrator out of a channel the
    /// other party is trying to open for them.
    pub fn grants_for(&self, peer: &PeerId) -> Vec<&KeyGrant> {
        let mut grants: Vec<&KeyGrant> = self
            .grants
            .iter()
            .filter(|grant| &grant.recipient == peer)
            .collect();
        grants.sort_by_key(|grant| std::cmp::Reverse(grant.granted_at.as_millis()));
        grants
    }

    /// The payment instructions written into this channel, in order.
    ///
    /// A list rather than one value: details legitimately change mid-trade
    /// (a merchant's first account bounces, they send a second), and a
    /// dispute turns entirely on *which* details were current when the
    /// buyer paid. Collapsing them to "the latest" would throw away the
    /// only record that answers that.
    pub fn payment_details(&self) -> Vec<&ChannelEntry> {
        self.entries_of_kind(EntryKind::PaymentDetails)
    }

    /// The conversation, in order.
    pub fn messages(&self) -> Vec<&ChannelEntry> {
        self.entries_of_kind(EntryKind::Message)
    }

    fn entries_of_kind(&self, kind: EntryKind) -> Vec<&ChannelEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.kind == kind)
            .collect()
    }

    /// The next sequence number `author` should write at.
    ///
    /// A convenience for a client that has just synced and needs to
    /// continue its own run without re-deriving it. Only ever consulted
    /// for one's own author id — asking it about the counterparty tells
    /// you how many entries they have written, which the entry list
    /// already says out loud.
    pub fn next_sequence(&self, author: &PeerId) -> u64 {
        self.entries
            .iter()
            .filter(|entry| &entry.author == author)
            .map(|entry| entry.sequence + 1)
            .max()
            .unwrap_or(0)
    }
}
