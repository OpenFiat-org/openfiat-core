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
    /// The signer is not a party to the settlement they are attaching to.
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
            // 6003 rather than a generic authorization code, of which
            // OFS-8000 has none: an attachment from someone who is not a
            // party to the trade is precisely evidence that is not valid.
            Self::NotAParty => ErrorCode::InvalidEvidence,
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
