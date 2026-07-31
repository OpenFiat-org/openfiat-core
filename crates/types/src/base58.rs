//! Base58 as the JSON representation of every raw-byte identifier.
//!
//! # Why these types do not use serde's derive
//!
//! `#[derive(Serialize)]` on a newtype over `[u8; 32]` or `Vec<u8>` renders
//! it in JSON as an array of numbers:
//!
//! ```text
//! "provider_public_key": [192, 74, 15, 103, 170, 216, ...]
//! ```
//!
//! That is wrong in three separate ways, and the first one is what makes it
//! urgent.
//!
//! **It is indistinguishable from a leaked secret.** An Ed25519 *private*
//! key is also 32 bytes. A reader looking at a field called
//! `provider_public_key` next to a bare array of 32 integers has no way to
//! tell, from the response alone, which one they are holding — the encoding
//! throws away the only clue. Every other system in this stack renders key
//! material as base58 precisely so that a public key *looks* like a public
//! key. This one did not, and a reader reasonably read it as a breach.
//!
//! **It is not the identifier anyone can use.** A node's `PeerId` in
//! `[0, 36, 8, 1, 18, 32, ...]` form is the same value as
//! `12D3KooWK9hQ7TwbfvFiaAxUbRFCkdhS7iEpAJDnewNL1anyREQ1`, but only the
//! second can be pasted into an `--entrypoint`, searched for in a log, or
//! compared against another node's by eye. An operator handed the array
//! form has to write a script to use their own node's identity.
//!
//! **It cannot be a map key.** JSON object keys are strings, so a
//! `HashMap<PublicKey, _>` could never be serialized at all while the array
//! encoding stood.
//!
//! # Why the compact format is deliberately left alone
//!
//! These same types cross the gossip wire and go to disk under `postcard`
//! (`openfiat_serialization::wire`), where nobody reads them and every byte
//! is paid for on every message. Base58 there would inflate a 32-byte key
//! to 44 bytes and buy nothing.
//!
//! So the split is on [`serde::Serializer::is_human_readable`]: JSON gets
//! base58, `postcard` keeps the exact bytes the derive produced. The
//! compact branch below forwards to `serialize_newtype_struct` rather than
//! `serialize_bytes` for that reason — it is what the derive emitted, so
//! the wire format is unchanged to the byte. `postcard_encoding_is_byte_for_byte_what_the_derive_produced`
//! in each type's test module is what holds that promise.
//!
//! # This does change what gets signed
//!
//! Every domain event is signed over `openfiat_serialization::json::to_bytes`
//! of its payload, and those payloads contain `PublicKey` fields. Moving
//! JSON to base58 therefore changes the signed transcript: an event signed
//! by a node running the previous encoding will not verify here, and vice
//! versa. That is a coordinated-upgrade change, not a compatible one. It is
//! taken deliberately — the alternative was a second, divergent JSON
//! representation used only at the RPC boundary, which is the arrangement
//! that produces "the same key, two different strings" bugs later.

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serializer};
use std::fmt;

/// Render `bytes` as base58 for JSON, or exactly as the derive would for a
/// compact format. `name` must be the type's own name, since that is what
/// `serialize_newtype_struct` was called with before.
pub(crate) fn serialize<S, B>(
    name: &'static str,
    bytes: &B,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    B: serde::Serialize + AsRef<[u8]>,
{
    if serializer.is_human_readable() {
        serializer.serialize_str(&bs58::encode(bytes.as_ref()).into_string())
    } else {
        serializer.serialize_newtype_struct(name, bytes)
    }
}

/// Decode a fixed 32-byte identifier, rejecting any base58 string that does
/// not decode to exactly 32 bytes.
pub(crate) fn deserialize_array32<'de, D>(
    name: &'static str,
    expecting: &'static str,
    deserializer: D,
) -> Result<[u8; 32], D::Error>
where
    D: Deserializer<'de>,
{
    if deserializer.is_human_readable() {
        let text = String::deserialize(deserializer)?;
        let bytes = decode(&text, expecting)?;
        let length = bytes.len();
        bytes
            .try_into()
            .map_err(|_| de::Error::invalid_length(length, &expecting))
    } else {
        deserializer.deserialize_newtype_struct(name, Array32Visitor(expecting))
    }
}

/// Decode a variable-length identifier. Length is not checked here: a
/// `PeerId` is a multihash whose length depends on the key type it wraps,
/// and a `Signature` deliberately carries whatever it was given so that a
/// malformed one fails verification rather than failing to parse.
pub(crate) fn deserialize_vec<'de, D>(
    name: &'static str,
    expecting: &'static str,
    deserializer: D,
) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    if deserializer.is_human_readable() {
        let text = String::deserialize(deserializer)?;
        decode(&text, expecting)
    } else {
        deserializer.deserialize_newtype_struct(name, VecVisitor(expecting))
    }
}

fn decode<E: de::Error>(text: &str, expecting: &'static str) -> Result<Vec<u8>, E> {
    bs58::decode(text)
        .into_vec()
        .map_err(|_| de::Error::invalid_value(de::Unexpected::Str(text), &expecting))
}

struct Array32Visitor(&'static str);

impl<'de> Visitor<'de> for Array32Visitor {
    type Value = [u8; 32];

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        <[u8; 32]>::deserialize(d)
    }
}

struct VecVisitor(&'static str);

impl<'de> Visitor<'de> for VecVisitor {
    type Value = Vec<u8>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        Vec::<u8>::deserialize(d)
    }
}
