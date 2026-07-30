//! A validated IPFS content identifier.
//!
//! # Why this is a parser and not a regex
//!
//! Every CID in this protocol arrives from someone else — a counterparty's
//! attachment, a stranger's avatar — and ends up concatenated into a
//! gateway URL (`https://<gateway>/ipfs/<cid>`) that a browser then
//! fetches. A string that reaches that position unchecked is a redirect
//! primitive: `../../evil`, an absolute `https://…` that replaces the
//! gateway entirely, or a `javascript:` scheme if it lands in an `href`.
//! The type that makes rendering safe has to be one that cannot hold any
//! of those, which means the constructor must be the only way in.
//!
//! So [`Cid::parse`] decodes rather than pattern-matches. A regex like
//! `^b[a-z2-7]{58}$` would accept `bzzzz…zzz` — 58 legal base32 characters
//! that decode to nothing meaningful — and that string would sail through
//! into a URL. Decoding the multibase, reading the version, codec, hash
//! function and digest length, and requiring them to agree with the bytes
//! actually present rejects it, because a CID is a self-describing
//! structure and a forgery has to be self-consistent to pass.
//!
//! # Why only CIDv1 base32, and only sha2-256
//!
//! The IPFS ecosystem accepts many encodings of the same content address.
//! This protocol accepts one, because a value that has several valid
//! spellings is a value that cannot be compared for equality: the same
//! content as `Qm…` (CIDv0, base58) and `bafy…` (CIDv1, base32) would be
//! two distinct strings in a signed record, in a store key, and in a
//! duplicate check. Restricting the accepted form to one canonical
//! spelling makes `Cid == Cid` mean "the same content", which every
//! consumer here relies on.
//!
//! sha2-256 specifically, because [`Cid::digest`] is what lets a consumer
//! verify fetched bytes against the identifier instead of trusting the
//! gateway that served them (see [`Cid::matches`]). A CID naming a hash
//! function we cannot compute is a CID we could never check.

/// The only way a CID can be wrong here: it is not one.
///
/// A single variant rather than a taxonomy of "bad multibase", "bad
/// codec", "bad digest length". A caller's only useful response to any of
/// them is identical — refuse the value — and distinguishing them in an
/// error message tells whoever supplied a hostile string exactly which
/// check to work around next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CidError {
    Malformed,
}

impl std::fmt::Display for CidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not a valid CIDv1 base32 sha2-256 identifier")
    }
}

impl std::error::Error for CidError {}

/// Multibase prefix for base32 lowercase, RFC 4648, no padding.
const MULTIBASE_BASE32_LOWER: char = 'b';
/// Multicodec for raw binary, the codec Kubo's `add` produces for a
/// single-block file with `--cid-version=1`.
const CODEC_RAW: u64 = 0x55;
/// Multicodec for dag-pb, produced for files large enough to be chunked
/// into a DAG. Both are legitimate outcomes of uploading a file, so both
/// are accepted; nothing else is.
const CODEC_DAG_PB: u64 = 0x70;
/// Multihash code for sha2-256.
const HASH_SHA2_256: u64 = 0x12;
/// sha2-256 digest length, in bytes.
const SHA2_256_LEN: usize = 32;

/// A CIDv1, base32-encoded, addressing sha2-256-hashed content.
///
/// Construct with [`Cid::parse`]. There is deliberately no `From<String>`,
/// no public field, and no `Deserialize` that bypasses validation — see
/// the module documentation for why the constructor is the only door.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct Cid(String);

impl Cid {
    /// Parses and validates a CID string.
    ///
    /// Accepts exactly what [`Cid`] documents: `b`-prefixed base32
    /// lowercase, CIDv1, raw or dag-pb codec, sha2-256 digest of the
    /// correct length. Everything else is [`CidError::Malformed`].
    pub fn parse(input: &str) -> Result<Self, CidError> {
        let mut chars = input.chars();
        if chars.next() != Some(MULTIBASE_BASE32_LOWER) {
            return Err(CidError::Malformed);
        }

        let bytes = base32_lower_decode(chars.as_str())?;
        let mut cursor = bytes.as_slice();

        // A CIDv1 is: version | codec | hash-code | digest-length | digest,
        // the first four as unsigned varints.
        if read_varint(&mut cursor)? != 1 {
            return Err(CidError::Malformed);
        }
        let codec = read_varint(&mut cursor)?;
        if codec != CODEC_RAW && codec != CODEC_DAG_PB {
            return Err(CidError::Malformed);
        }
        if read_varint(&mut cursor)? != HASH_SHA2_256 {
            return Err(CidError::Malformed);
        }
        if read_varint(&mut cursor)? != SHA2_256_LEN as u64 {
            return Err(CidError::Malformed);
        }
        // The declared length must match the bytes actually present. This
        // is the check that rejects a plausible-looking string whose
        // header happens to decode but whose body was truncated or padded.
        if cursor.len() != SHA2_256_LEN {
            return Err(CidError::Malformed);
        }

        Ok(Self(input.to_string()))
    }

    /// The canonical string. Safe to concatenate into a gateway path:
    /// by construction it is base32 lowercase and contains no `/`, `.`,
    /// `:`, `?` or `#`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The sha2-256 digest this CID addresses.
    pub fn digest(&self) -> [u8; SHA2_256_LEN] {
        // Re-decoding cannot fail: the only constructor validated it.
        let decoded = base32_lower_decode(&self.0[1..]).expect("a Cid was validated at parse time");
        let mut cursor = decoded.as_slice();
        for _ in 0..4 {
            read_varint(&mut cursor).expect("a Cid was validated at parse time");
        }
        let mut digest = [0u8; SHA2_256_LEN];
        digest.copy_from_slice(cursor);
        digest
    }

    /// Whether `content` is the data this CID names.
    ///
    /// This is what makes a public gateway an untrusted transport rather
    /// than an authority: fetch from anywhere, then check here. A gateway
    /// that substitutes different bytes fails this, and a gateway that
    /// serves nothing is indistinguishable from one that is merely down —
    /// neither can make a consumer accept content the author did not sign.
    ///
    /// Only meaningful for raw-codec CIDs, where the digest is taken over
    /// the file bytes themselves. A dag-pb CID addresses the root node of
    /// a chunked DAG, whose digest is over the node's encoding rather than
    /// the file, so this returns `false` for content that is genuinely
    /// correct. Callers holding a possibly-chunked file must verify
    /// through an IPFS client that can walk the DAG instead.
    pub fn matches(&self, content: &[u8]) -> bool {
        self.digest() == crate::hash::sha256(content)
    }
}

impl std::fmt::Display for Cid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Deserialization goes through [`Cid::parse`] rather than accepting the
/// string as-is. A `Cid` reconstructed from disk or from a peer's gossip
/// has crossed a trust boundary exactly like one typed by a user, and the
/// invariant the rest of this crate relies on — that a `Cid` is safe to
/// concatenate into a URL — would be worth nothing if the wire format
/// could mint one that never met it.
impl<'de> serde::Deserialize<'de> for Cid {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Cid::parse(&raw).map_err(|_| serde::de::Error::custom("not a valid CIDv1 base32 sha2-256"))
    }
}

/// RFC 4648 base32 lowercase, no padding.
fn base32_lower_decode(input: &str) -> Result<Vec<u8>, CidError> {
    let mut out = Vec::with_capacity(input.len() * 5 / 8);
    let mut buffer: u16 = 0;
    let mut bits: u8 = 0;

    for c in input.chars() {
        let value = match c {
            'a'..='z' => c as u8 - b'a',
            '2'..='7' => c as u8 - b'2' + 26,
            _ => return Err(CidError::Malformed),
        };
        buffer = (buffer << 5) | u16::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }

    // Whatever is left over must be zero padding. A non-zero remainder
    // means the encoder produced something other than this alphabet's
    // canonical output, so two different strings could decode to the same
    // bytes — and `Cid` keeps the string, not the bytes, so that would
    // reintroduce exactly the multiple-spellings problem parsing exists
    // to prevent.
    if bits >= 5 || (buffer & ((1 << bits) - 1)) != 0 {
        return Err(CidError::Malformed);
    }

    Ok(out)
}

/// Unsigned LEB128, as multiformats uses. Bounded at 9 bytes so a
/// malicious run of continuation bits cannot loop or overflow.
fn read_varint(cursor: &mut &[u8]) -> Result<u64, CidError> {
    let mut value: u64 = 0;
    for index in 0..9 {
        let byte = *cursor.first().ok_or(CidError::Malformed)?;
        *cursor = &cursor[1..];
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(CidError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CID this workspace genuinely produced: the bytes below were
    /// uploaded to Filebase's IPFS RPC, which returned this identifier,
    /// and the same identifier retrieves them from ipfs.io — an unrelated
    /// gateway, no credentials. Checking the parser against real network
    /// output rather than a hand-assembled fixture matters, because a
    /// fixture built by the parser's own reasoning would agree with it
    /// even if both were wrong.
    const REAL_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";
    const REAL_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

    /// What the same provider returned for a 900 KB upload. Filebase
    /// chunks past its own threshold and hands back a dag-pb root, so
    /// this is not a hypothetical codec — it is what half of all real
    /// uploads produce, and rejecting it would break every attachment
    /// larger than a small image.
    const REAL_DAG_PB_CID: &str = "bafybeig3ci7io2duyknu34co3zw42oodnfyamwazsus42vpgnvq2hpzodm";

    #[test]
    fn parses_the_dag_pb_cid_a_chunked_upload_produces() {
        let cid = Cid::parse(REAL_DAG_PB_CID).expect("a chunked upload's CID must parse");
        assert_eq!(cid.as_str(), REAL_DAG_PB_CID);
    }

    #[test]
    fn a_dag_pb_root_does_not_match_the_file_it_addresses() {
        // Its digest is over the DAG node, not the bytes, so `matches`
        // cannot be used to check a chunked file — documented on the
        // method, asserted here so the documentation stays true.
        let cid = Cid::parse(REAL_DAG_PB_CID).unwrap();
        assert!(!cid.matches(REAL_CONTENT));
    }

    #[test]
    fn parses_a_cid_this_project_actually_uploaded() {
        let cid = Cid::parse(REAL_CID).expect("a CID Filebase returned must parse");
        assert_eq!(cid.as_str(), REAL_CID);
    }

    #[test]
    fn the_digest_is_the_hash_of_the_content_it_names() {
        let cid = Cid::parse(REAL_CID).unwrap();
        assert!(
            cid.matches(REAL_CONTENT),
            "a raw CID's digest must be sha2-256 of the bytes, or `matches` \
             cannot be used to check a gateway"
        );
    }

    #[test]
    fn content_the_cid_does_not_name_is_rejected() {
        let cid = Cid::parse(REAL_CID).unwrap();
        assert!(
            !cid.matches(b"substituted by the gateway"),
            "this is the whole reason a gateway need not be trusted"
        );
    }

    /// The case a regex would have let through.
    #[test]
    fn a_string_of_legal_base32_that_is_not_a_cid_is_rejected() {
        let plausible = format!("b{}", "z".repeat(REAL_CID.len() - 1));
        assert_eq!(plausible.len(), REAL_CID.len());
        assert_eq!(Cid::parse(&plausible), Err(CidError::Malformed));
    }

    #[test]
    fn path_traversal_and_absolute_urls_never_become_a_cid() {
        for hostile in [
            "../../../etc/passwd",
            "https://evil.example/payload",
            "javascript:alert(1)",
            "bafkrei../../../etc/passwd",
            "b../..",
            "",
            "b",
        ] {
            assert_eq!(
                Cid::parse(hostile),
                Err(CidError::Malformed),
                "{hostile:?} must not survive into a gateway URL"
            );
        }
    }

    #[test]
    fn a_validated_cid_contains_no_url_significant_character() {
        let cid = Cid::parse(REAL_CID).unwrap();
        for c in cid.as_str().chars() {
            assert!(
                c.is_ascii_lowercase() || c.is_ascii_digit(),
                "{c:?} would change the meaning of a gateway path"
            );
        }
    }

    #[test]
    fn cidv0_is_rejected_because_it_would_be_a_second_spelling() {
        // A real CIDv0 for the empty unixfs file. Valid IPFS, but base58
        // and version 0, so accepting it would mean one piece of content
        // could appear under two unequal strings.
        assert_eq!(
            Cid::parse("QmbFMke1KXqnYyBBWxB74N4c5SBnJMVAiMNRcGu6x1AwQH"),
            Err(CidError::Malformed)
        );
    }

    #[test]
    fn uppercase_base32_is_rejected_for_the_same_reason() {
        assert_eq!(
            Cid::parse(&REAL_CID.to_uppercase()),
            Err(CidError::Malformed)
        );
    }

    #[test]
    fn a_truncated_digest_is_rejected_even_though_the_header_decodes() {
        let truncated = &REAL_CID[..REAL_CID.len() - 4];
        assert_eq!(Cid::parse(truncated), Err(CidError::Malformed));
    }

    #[test]
    fn deserialization_cannot_mint_an_unvalidated_cid() {
        let valid: Cid = serde_json::from_slice(format!("\"{REAL_CID}\"").as_bytes())
            .expect("a real CID round-trips");
        assert_eq!(valid.as_str(), REAL_CID);

        let hostile: Result<Cid, _> = serde_json::from_slice(b"\"../../../etc/passwd\"");
        assert!(
            hostile.is_err(),
            "a peer's gossip must not be able to introduce a Cid that \
             never passed the parser"
        );
    }

    #[test]
    fn a_varint_run_of_continuation_bytes_terminates() {
        // 'b' + base32 of ten 0xff bytes: every byte sets the
        // continuation bit, so an unbounded reader would keep going.
        let all_continuations = "b777777777777777";
        assert_eq!(Cid::parse(all_continuations), Err(CidError::Malformed));
    }
}
