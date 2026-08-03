//! Trade-channel failures, mapped onto OFS-8000 codes that already exist.
//!
//! No new code is minted here: `openfiat-types`' `ErrorCode` is the
//! protocol's shared vocabulary and a number invented in one crate would
//! mean nothing to anyone reading it from an SDK.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeChannelError {
    InvalidSignature,
    /// The channel hangs off a settlement, and this node has not seen it.
    /// Not necessarily an attack: a node that has not caught up yet will
    /// legitimately see an entry before the settlement it belongs to.
    SettlementNotFound,
    /// The author or granter is neither the buyer nor the seller of the
    /// settlement this channel belongs to.
    NotAParty,
    /// A grant addressed to someone who is not a party and not an
    /// arbitrator on an open dispute over this settlement — see
    /// `TradeChannelRegistry::apply_key_grant` for why the readership is
    /// constrained rather than left to the granter.
    RecipientNotPermitted,
    /// The payload exceeds [`crate::key::MAX_ENTRY_CIPHERTEXT`].
    EntryTooLarge,
    /// The author has already written a different entry at this sequence
    /// number. Re-posting the identical entry is not an error — gossip
    /// delivers the same event more than once as a matter of course.
    SequenceReused,
    MalformedEntry,
    /// Decryption failed. Deliberately collapses "wrong key", "tampered
    /// ciphertext" and "payload lifted from another slot" into one
    /// variant, for the same reason `openfiat_crypto::SealError` does:
    /// telling them apart is an oracle.
    PayloadDidNotOpen,
}

impl TradeChannelError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            // The settlement range's own code rather than the generic
            // `ResourceNotFound` this used to answer with — see
            // `openfiat_disputes`'s note on why the three crates that
            // raise this condition now answer it identically.
            Self::SettlementNotFound => ErrorCode::SettlementNotFound,
            Self::NotAParty | Self::RecipientNotPermitted => ErrorCode::InvalidIdentityClaim,
            Self::EntryTooLarge => ErrorCode::InvalidParameter,
            Self::SequenceReused => ErrorCode::ResourceAlreadyExists,
            Self::MalformedEntry => ErrorCode::DeserializationError,
            Self::PayloadDidNotOpen => ErrorCode::InvalidSignature,
        }
    }
}

impl fmt::Display for TradeChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for TradeChannelError {}
