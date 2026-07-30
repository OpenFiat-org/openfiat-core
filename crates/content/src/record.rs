//! What an attachment is, and what it deliberately is not.
//!
//! # Anyone can read this
//!
//! Content behind a CID is public. This was not assumed — it was checked:
//! a file pinned through this project's own Filebase credentials was
//! retrieved from `ipfs.io`, an unrelated gateway, by CID alone with no
//! token of any kind. A CID is a capability that anybody who sees it can
//! exercise, and CIDs travel in signed gossip that every node on the
//! network stores.
//!
//! So an attachment here is **public evidence**, and the interface must
//! say so where the user picks the file, not in a policy document. That is
//! not a limitation of IPFS to be worked around later with an encryption
//! flag: the whole value of an attachment in a dispute is that an
//! arbitrator drawn by sortition — someone neither party chose, whose
//! identity is not known at upload time — can read it. Evidence only the
//! author can decrypt is not evidence.
//!
//! Private payment details are a different problem with a different
//! answer (sealed exchange between counterparties), and are deliberately
//! not routed through this crate.
//!
//! # Why the media type is an enum
//!
//! `media_type` decides how a viewer renders the bytes. A free-form
//! string there lets an author choose `text/html`, and a gateway that
//! honours it serves attacker-authored HTML from a host the user was told
//! to trust — script included, on whatever origin the gateway sits on.
//! The closed set below is the set this protocol will render, and it
//! contains no format that can execute. SVG is excluded for exactly this
//! reason despite being an image: it is a document with scripting.

use crate::error::ContentError;
use openfiat_crypto::Cid;
use openfiat_settlement::SettlementId;
use openfiat_types::{PeerId, PublicKey, Timestamp};

/// The largest attachment a node will accept a record for, and the cap a
/// client should refuse to fetch past.
///
/// Enforced against the author's declared [`Attachment::size_bytes`],
/// which is a claim rather than a measurement — nothing in a signed
/// record can prove the size of data held somewhere else. The real
/// defence is that a consumer streams with its own limit and stops; this
/// constant is what makes an obviously-absurd claim rejectable at the
/// point of publication rather than at the point of download.
pub const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

/// The formats this protocol renders. See the module documentation for
/// why this is a closed set and why SVG is not in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MediaType {
    Png,
    Jpeg,
    Webp,
    Pdf,
}

impl MediaType {
    /// The IANA type, for a `Content-Type` header or an `accept` filter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Pdf => "application/pdf",
        }
    }

    /// Parses a browser-supplied MIME type. Rejects anything outside the
    /// set rather than falling back to a default, because a default here
    /// would mean an unrecognised type renders as *something*.
    pub fn parse(input: &str) -> Result<Self, ContentError> {
        match input {
            "image/png" => Ok(Self::Png),
            "image/jpeg" => Ok(Self::Jpeg),
            "image/webp" => Ok(Self::Webp),
            "application/pdf" => Ok(Self::Pdf),
            _ => Err(ContentError::UnsupportedMediaType),
        }
    }

    /// Whether a viewer may put this in an `<img>`. A PDF may not: it is
    /// a document format handed to a plugin, not a bitmap.
    pub const fn is_image(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::Webp)
    }

    /// The first bytes every file of this type begins with.
    ///
    /// Checked at upload because the browser-supplied MIME type is just a
    /// string the client chose: a caller can label anything `image/png`.
    /// The magic number is a property of the bytes themselves, so this is
    /// the difference between believing the uploader and checking.
    pub const fn magic(self) -> &'static [u8] {
        match self {
            Self::Png => b"\x89PNG\r\n\x1a\n",
            Self::Jpeg => &[0xFF, 0xD8, 0xFF],
            // RIFF....WEBP — the four size bytes in between are why only
            // the leading tag is matched here.
            Self::Webp => b"RIFF",
            Self::Pdf => b"%PDF-",
        }
    }

    /// Whether `content` actually begins like this media type.
    pub fn looks_like(self, content: &[u8]) -> bool {
        let magic = self.magic();
        if !content.starts_with(magic) {
            return false;
        }
        // RIFF is a container tag shared with WAV and AVI, so the WEBP
        // form-type at offset 8 is what distinguishes an image from audio
        // a caller mislabelled.
        if matches!(self, Self::Webp) {
            return content.len() >= 12 && &content[8..12] == b"WEBP";
        }
        true
    }
}

/// What an attachment is attached to.
///
/// A settlement, not a dispute: evidence is gathered during a trade and
/// often before anyone opens a dispute, and forcing an upload to name a
/// dispute would mean nothing could be attached until the trade had
/// already gone wrong. A dispute references its settlement, so an
/// arbitrator reaches the same set either way.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AttachmentSubject {
    Settlement(SettlementId),
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct AttachmentId(String);

impl AttachmentId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One published attachment.
///
/// Immutable. There is no edit and no delete: a record that could be
/// withdrawn after an arbitrator read it would let a party show evidence
/// and then remove it from the record, and the counterparty's copy of the
/// gossip would disagree with the author's. Unpinning the content is the
/// author's own business and makes the CID unresolvable, but the *record*
/// that they published it stands.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Attachment {
    pub id: AttachmentId,
    pub subject: AttachmentSubject,
    pub author: PeerId,
    pub author_public_key: PublicKey,
    pub cid: Cid,
    pub media_type: MediaType,
    /// The author's declared size. Advisory — see [`MAX_ATTACHMENT_BYTES`].
    pub size_bytes: u64,
    /// A short author-supplied label ("bank transfer receipt"). Rendered
    /// as text, never as markup.
    pub caption: String,
    pub created_at: Timestamp,
}

/// Longest accepted [`Attachment::caption`], in characters. A caption is
/// a label beside a thumbnail, and an unbounded string in a gossiped
/// record is a way to make every node on the network store a megabyte per
/// upload.
pub const MAX_CAPTION_CHARS: usize = 200;

impl Attachment {
    /// The checks that do not need any state: shape, not authorization.
    ///
    /// Kept separate from the store's checks so a client can run it
    /// before signing, and so the reason a record was refused is either
    /// "this is malformed" or "you are not a party", never both at once.
    pub fn validate(&self) -> Result<(), ContentError> {
        if self.size_bytes == 0 || self.size_bytes > MAX_ATTACHMENT_BYTES {
            return Err(ContentError::TooLarge);
        }
        if self.caption.chars().count() > MAX_CAPTION_CHARS {
            return Err(ContentError::MalformedAttachment);
        }
        if self.id.as_str().is_empty() {
            return Err(ContentError::MalformedAttachment);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_renderable_media_type_is_inert() {
        for media in [
            MediaType::Png,
            MediaType::Jpeg,
            MediaType::Webp,
            MediaType::Pdf,
        ] {
            assert!(
                !media.as_str().contains("html") && !media.as_str().contains("svg"),
                "{} can carry script and must not be renderable",
                media.as_str()
            );
        }
    }

    #[test]
    fn an_executable_type_cannot_be_named_at_all() {
        for hostile in [
            "image/svg+xml",
            "text/html",
            "application/javascript",
            "text/html; charset=utf-8",
            "image/png; charset=utf-8",
        ] {
            assert_eq!(
                MediaType::parse(hostile),
                Err(ContentError::UnsupportedMediaType),
                "{hostile:?} must not become a MediaType"
            );
        }
    }

    #[test]
    fn a_pdf_is_never_treated_as_an_image() {
        assert!(!MediaType::Pdf.is_image());
        assert!(MediaType::Png.is_image());
    }

    #[test]
    fn content_mislabelled_as_an_image_fails_the_magic_number_check() {
        let html = b"<script>alert(1)</script>";
        for media in [
            MediaType::Png,
            MediaType::Jpeg,
            MediaType::Webp,
            MediaType::Pdf,
        ] {
            assert!(
                !media.looks_like(html),
                "a caller claiming {} for HTML must be caught by the bytes",
                media.as_str()
            );
        }
    }

    #[test]
    fn real_headers_are_recognised() {
        assert!(MediaType::Png.looks_like(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"));
        assert!(MediaType::Jpeg.looks_like(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]));
        assert!(MediaType::Pdf.looks_like(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3"));
        assert!(MediaType::Webp.looks_like(b"RIFF\x24\x00\x00\x00WEBPVP8 "));
    }

    #[test]
    fn a_wav_file_is_not_accepted_as_a_webp() {
        // Same RIFF container tag, different form type. Matching only the
        // leading four bytes would have let this through.
        assert!(!MediaType::Webp.looks_like(b"RIFF\x24\x00\x00\x00WAVEfmt "));
    }

    fn attachment(size: u64, caption: &str) -> Attachment {
        Attachment {
            id: AttachmentId::new("att-1"),
            subject: AttachmentSubject::Settlement(SettlementId::new("s-1")),
            author: PeerId::from_bytes(vec![1; 8]),
            author_public_key: PublicKey::from_bytes([2; 32]),
            cid: crate::fixtures::probe_cid(),
            media_type: MediaType::Png,
            size_bytes: size,
            caption: caption.to_string(),
            created_at: Timestamp::from_millis(1),
        }
    }

    #[test]
    fn a_reasonable_attachment_validates() {
        assert_eq!(attachment(1_024, "receipt").validate(), Ok(()));
    }

    #[test]
    fn an_oversized_or_empty_declaration_is_refused() {
        assert_eq!(
            attachment(MAX_ATTACHMENT_BYTES + 1, "x").validate(),
            Err(ContentError::TooLarge)
        );
        assert_eq!(attachment(0, "x").validate(), Err(ContentError::TooLarge));
    }

    #[test]
    fn an_unbounded_caption_cannot_be_pushed_to_every_node() {
        let huge = "a".repeat(MAX_CAPTION_CHARS + 1);
        assert_eq!(
            attachment(10, &huge).validate(),
            Err(ContentError::MalformedAttachment)
        );
    }
}
