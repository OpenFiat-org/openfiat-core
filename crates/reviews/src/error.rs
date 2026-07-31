//! Review failures.
//!
//! Every variant maps onto a code OFS-8000 already defines. None is
//! invented here: the error registry is part of the wire contract that
//! third-party clients switch on, so a code that exists only in this
//! crate would be a code no SDK could recognise. Where the closest
//! existing code is imperfect the mapping says why.

use openfiat_types::ErrorCode;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewError {
    InvalidSignature,
    /// The signer is not a party to the settlement they are reviewing, or
    /// the settlement never reached a settled state. The two are one
    /// variant because they are one question — "may this wallet review
    /// this trade?" — answered by [`crate::record::subject_of`].
    NotAParty,
    /// This author has already reviewed this settlement, and the review on
    /// file is the one that stands. See
    /// [`crate::store::ReviewRegistry::apply_publish`] for which of two
    /// competing records wins and why the rule cannot be "whichever
    /// arrived first".
    AlreadyReviewed,
    /// The comment is longer than [`crate::record::MAX_COMMENT_CHARS`], or
    /// carries characters that would let it misrender.
    MalformedReview,
}

impl ReviewError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            // OFS-8000 has no general authorization code. 2001 is the
            // closest true statement: the record claims a standing —
            // "I am a party to this trade" — that this node checked
            // against the settlement and found false.
            Self::NotAParty => ErrorCode::InvalidIdentityClaim,
            Self::AlreadyReviewed => ErrorCode::ResourceAlreadyExists,
            Self::MalformedReview => ErrorCode::InvalidParameter,
        }
    }
}

impl fmt::Display for ReviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for ReviewError {}
