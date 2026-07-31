//! `openfiat-tradechannel` — the confidential two-party channel attached
//! to a settlement: the payment details one party must hand the other,
//! and the conversation they have while the trade runs.
//!
//! # The constraint everything here answers to
//!
//! Every protocol event in this workspace is gossiped to every node and
//! kept forever. A seller's bank account number cannot travel that way in
//! the clear, and neither can a conversation. "Sealed" therefore has to
//! mean *encrypted to the counterparty* — not "the read handler checks
//! who is asking", which protects nothing from the several hundred
//! machines that already hold a replica.
//!
//! # The mechanism, and why it is the existing one
//!
//! `openfiat-notifications` solved the identical problem for delivery
//! destinations: seal the secret to the recipient's published identity
//! key with `openfiat_crypto::seal`, gossip the ciphertext, and let
//! exactly one private key open it. This crate uses that unchanged — see
//! [`record::KeyGrant`], which is a `SealedBox` and nothing more.
//!
//! What it adds is one indirection, and only because a trade needs
//! something a notification does not: **an audience that grows after the
//! ciphertext was written.** An arbitrator who must read a disputed
//! trade's conversation is not known, and does not exist, while the
//! parties are talking. A sealed box cannot address them.
//!
//! So the content is encrypted once under a random per-trade key, and
//! that 32-byte key is what gets sealed — to the buyer, to the seller,
//! and later, if a party chooses, to an arbitrator who has joined the
//! dispute. See [`key`] for the format and for why re-sealing the
//! *messages* to an arbitrator instead would have been strictly worse:
//! it would put the disclosing party in charge of re-encrypting the
//! transcript, and an arbitrator would then be reading whatever that
//! party produced after the argument started, rather than the original
//! signed ciphertexts the whole network already replicated.
//!
//! # What each observer can actually see
//!
//! **A node operator**, holding a full replica: that a channel exists and
//! for which settlement; who granted the key to whom, and when; who wrote
//! each entry, in what order, at what claimed time, whether it is payment
//! details or chat, and what size it padded to. Not the details. Not one
//! word of the conversation. The metadata is real and is written down
//! rather than glossed over — a replicated log cannot carry an entry
//! without carrying the fact that it exists.
//!
//! **An arbitrator**, once they have joined a dispute *and* a party has
//! granted them the key: the entire channel, from before the dispute
//! existed — every message and every payment detail, in one grant. There
//! is deliberately no partial disclosure: a party who could hand over
//! three of five messages would be handing over an argument, not
//! evidence. If neither party grants, the arbitrator sees exactly what a
//! node operator sees, and everyone can see that no grant was made.
//!
//! **A party**: everything, forever, including after the trade closes. A
//! grant cannot be revoked — the recipient already holds bytes that are
//! replicated everywhere — and this crate does not pretend otherwise.
//!
//! # What this deliberately does not provide
//!
//! *Forward secrecy.* One long-lived key per trade, sealed under
//! long-lived identity keys. Compromising a party's wallet key exposes
//! every channel that wallet was ever in. That is not an oversight being
//! deferred: the requirement is that a third party nobody can name yet
//! must be able to read this later, which is the exact opposite of
//! forward secrecy. The two cannot both be had, and the dispute
//! requirement is the one the protocol is for.
//!
//! *Presence and typing indicators.* Scoped out, with reasons, in
//! `docs/trade-channel.md`: a "typing" event gossiped to every node and
//! stored forever is the wrong shape for an ephemeral signal, and its
//! metadata — who was awake, on which trade, at which second — is
//! precisely what a permanently replicated log should not accumulate.

pub mod error;
pub mod events;
pub mod key;
pub mod protocol;
pub mod record;
pub mod service;
pub mod store;

pub use error::TradeChannelError;
pub use key::{
    ChannelCiphertext, ChannelKey, ChannelKeyId, EntryBinding, MAX_ENTRY_CIPHERTEXT,
    MAX_ENTRY_PLAINTEXT, open_entry, seal_entry,
};
pub use protocol::COLUMN_FAMILIES;
pub use record::{ChannelEntry, EntryKind, GrantRole, KeyGrant, TradeChannel};
pub use service::TradeChannelService;
pub use store::TradeChannelRegistry;

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
