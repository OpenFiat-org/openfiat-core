//! Content failures.
//!
//! Every variant maps onto a code OFS-8000 already defines. None is
//! invented here: the error registry is part of the wire contract that
//! third-party clients switch on, so a code that exists only in this
//! crate would be a code no SDK could recognise. Where the closest
//! existing code is imperfect the mapping says why.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentError {
    /// Not a CIDv1 base32 sha2-256 identifier. See [`crate::Cid`] for why
    /// the accepted form is this narrow.
    MalformedCid,
    /// A media type outside the set this protocol will render.
    UnsupportedMediaType,
    /// The author's declared size exceeds [`crate::MAX_ATTACHMENT_BYTES`].
    TooLarge,
    InvalidSignature,
    /// The author's claimed [`openfiat_types::PeerId`] is not the one
    /// their own public key derives to.
    ///
    /// Named for the authorization it was meant to enforce, but the
    /// check it actually performs is in [`crate::events`] and is about
    /// self-consistency: whether this record's two identity fields agree
    /// with each other. Whether the signer is a party to the settlement
    /// is decided at read time by
    /// [`crate::store::AttachmentRegistry::find_by_settlement`], which
    /// filters rather than errors.
    NotAParty,
    DuplicateAttachmentId,
    AttachmentNotFound,
    MalformedAttachment,
}

impl ContentError {
    pub const fn code(self) -> ErrorCode {
        match self {
            // All three are "the request named something this protocol
            // does not accept", which is what InvalidParameter is for.
            Self::MalformedCid | Self::UnsupportedMediaType | Self::TooLarge => {
                ErrorCode::InvalidParameter
            }
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            // 2001, not `InvalidEvidence` (6003), and the old mapping's
            // stated reasoning was wrong on both halves.
            //
            // It claimed OFS-8000 has no generic authorization code.
            // 2001 is exactly that code, and it is what the three
            // neighbours raising this same condition already use —
            // `openfiat_reviews`, `openfiat_tradechannel` and
            // `openfiat_disputes` all answer `NotAParty` with it.
            //
            // It then claimed the caller is not a party to the trade.
            // The check this variant guards does not look at the trade
            // at all: it compares the record's claimed author against
            // the Peer ID its own public key derives to — the same
            // condition `DiscoveryError::PeerIdMismatch` reports, also
            // as 2001. A false identity claim, made by whoever signed.
            //
            // 6003 meanwhile says the attachment is bad, and an author
            // who believes that re-encodes, re-uploads and re-pins a
            // file that was never the problem.
            Self::NotAParty => ErrorCode::InvalidIdentityClaim,
            Self::DuplicateAttachmentId => ErrorCode::ResourceAlreadyExists,
            Self::AttachmentNotFound => ErrorCode::ResourceNotFound,
            Self::MalformedAttachment => ErrorCode::DeserializationError,
        }
    }
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for ContentError {}
