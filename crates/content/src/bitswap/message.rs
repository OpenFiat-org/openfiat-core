//! The bitswap 1.2.0 wire message.
//!
//! # Why this is written out rather than depended on
//!
//! Every published Rust bitswap crate pins libp2p 0.53 or older
//! (`libp2p-bitswap` 0.25 → 0.50, `libp2p-bitswap-next` 0.26 → 0.53,
//! `beetle-bitswap-next` 0.5 → 0.53). This workspace is on 0.56, and two
//! libp2p versions in one binary are two incompatible `NetworkBehaviour`
//! traits — a swarm cannot hold a behaviour built against the other one,
//! so this is a compile error rather than a risk. Checking that first was
//! the point; the wire format below is the consequence.
//!
//! It is not much to own. Bitswap's message is six protobuf message types
//! with no nesting deeper than two, and the parts a node actually needs
//! are narrower still: read what a peer wants, answer with blocks or with
//! "I do not have that".
//!
//! # What is deliberately not modelled
//!
//! Field 2 of `Message` is bitswap 1.0/1.1's bare `repeated bytes blocks`,
//! superseded by the prefixed `payload` in 1.1. Unknown and superseded
//! fields are skipped by wire type rather than rejected, which is what
//! makes this forward-compatible with a peer speaking a later revision —
//! protobuf's own contract, and the reason a stricter parser here would
//! break against implementations that are not wrong.

use openfiat_crypto::Cid;

/// The largest single message this node will read from a peer.
///
/// go-bitswap's own ceiling is 4 MiB; this is lower because nothing this
/// protocol serves approaches it — a held block is capped at
/// [`crate::held::MAX_BLOCK_BYTES`] (256 KiB) and a wantlist of any sane
/// size fits many times over. The cap exists because the length prefix
/// arrives before the body: without one, a peer sending a 4-byte varint
/// claiming 4 GiB would have this node allocate it.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

/// The most wantlist entries answered from one message.
///
/// A peer may legitimately want many blocks, but each entry answered
/// costs a store lookup and possibly a block copied into a response. This
/// bounds the work one message can ask for; entries past it are ignored,
/// and a peer that genuinely wants more will ask again — bitswap
/// wantlists are resent, so nothing is lost by not answering all of one
/// message.
pub const MAX_WANTLIST_ENTRIES: usize = 1024;

/// What a peer is asking for: the block itself, or only whether we hold
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WantType {
    Block,
    Have,
}

/// One wantlist entry, reduced to the fields a responder acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub cid: Cid,
    pub want_type: WantType,
    /// A cancellation, not a request. The peer no longer wants this.
    pub cancel: bool,
    /// Whether the peer wants to be told when we do *not* have it.
    ///
    /// Silence is bitswap's default answer to a block one does not hold,
    /// because on the open network most peers hold most things not at
    /// all. A peer that sets this is asking to be told so it can stop
    /// waiting — answering it is what makes a fetch fail fast rather than
    /// time out.
    pub send_dont_have: bool,
}

/// Whether a peer holds a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Have,
    DontHave,
}

/// A parsed bitswap message.
///
/// Only the parts this node acts on survive parsing: entries whose CID is
/// not one this protocol accepts are dropped rather than represented,
/// because a `Cid` that never passed its parser is exactly what the rest
/// of this workspace is built to never hold.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Message {
    pub wants: Vec<Want>,
    /// Blocks the peer sent us, each already re-addressed from its own
    /// prefix and data — see [`Cid::from_binary`]. A block whose
    /// rebuilt CID this protocol does not accept is dropped here.
    pub blocks: Vec<(Cid, Vec<u8>)>,
    pub presences: Vec<(Cid, Presence)>,
}

impl Message {
    pub fn is_empty(&self) -> bool {
        self.wants.is_empty() && self.blocks.is_empty() && self.presences.is_empty()
    }

    /// Encodes to the protobuf a bitswap peer expects.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();

        if !self.wants.is_empty() {
            let mut wantlist = Vec::new();
            for want in &self.wants {
                let mut entry = Vec::new();
                write_bytes(&mut entry, 1, &want.cid.to_binary());
                if want.cancel {
                    write_varint_field(&mut entry, 3, 1);
                }
                if want.want_type == WantType::Have {
                    write_varint_field(&mut entry, 4, 1);
                }
                if want.send_dont_have {
                    write_varint_field(&mut entry, 5, 1);
                }
                write_bytes(&mut wantlist, 1, &entry);
            }
            write_bytes(&mut out, 1, &wantlist);
        }

        for (cid, data) in &self.blocks {
            let mut block = Vec::new();
            write_bytes(&mut block, 1, &cid.prefix());
            write_bytes(&mut block, 2, data);
            write_bytes(&mut out, 3, &block);
        }

        for (cid, presence) in &self.presences {
            let mut entry = Vec::new();
            write_bytes(&mut entry, 1, &cid.to_binary());
            if *presence == Presence::DontHave {
                write_varint_field(&mut entry, 2, 1);
            }
            write_bytes(&mut out, 4, &entry);
        }

        out
    }

    /// Parses a message, keeping only what this node can act on.
    ///
    /// Returns `None` for input that is not protobuf at all. Content this
    /// protocol does not accept — a CID under an unsupported hash, a
    /// block whose bytes do not match the prefix it arrived with — is
    /// dropped from the result rather than failing the whole message: one
    /// unusable entry among a hundred is a peer with broader tastes, not
    /// a peer speaking nonsense.
    pub fn decode(input: &[u8]) -> Option<Self> {
        let mut message = Message::default();
        let mut reader = Reader::new(input);

        while let Some((field, wire)) = reader.tag()? {
            match (field, wire) {
                (1, 2) => decode_wantlist(reader.length_delimited()?, &mut message)?,
                (3, 2) => decode_block(reader.length_delimited()?, &mut message)?,
                (4, 2) => decode_presence(reader.length_delimited()?, &mut message)?,
                _ => reader.skip(wire)?,
            }
        }
        Some(message)
    }
}

fn decode_wantlist(input: &[u8], message: &mut Message) -> Option<()> {
    let mut reader = Reader::new(input);
    while let Some((field, wire)) = reader.tag()? {
        match (field, wire) {
            (1, 2) => {
                let entry = reader.length_delimited()?;
                if message.wants.len() < MAX_WANTLIST_ENTRIES
                    && let Some(want) = decode_want(entry)
                {
                    message.wants.push(want);
                }
            }
            _ => reader.skip(wire)?,
        }
    }
    Some(())
}

fn decode_want(input: &[u8]) -> Option<Want> {
    let mut reader = Reader::new(input);
    let mut cid = None;
    let mut want_type = WantType::Block;
    let mut cancel = false;
    let mut send_dont_have = false;

    while let Some((field, wire)) = reader.tag()? {
        match (field, wire) {
            // A CID this protocol does not accept leaves `cid` unset, so
            // the entry is dropped below — the entry is still consumed,
            // keeping the rest of the wantlist readable.
            (1, 2) => cid = Cid::from_binary(reader.length_delimited()?).ok(),
            (3, 0) => cancel = reader.varint()? != 0,
            (4, 0) => {
                want_type = if reader.varint()? == 1 {
                    WantType::Have
                } else {
                    WantType::Block
                }
            }
            (5, 0) => send_dont_have = reader.varint()? != 0,
            _ => reader.skip(wire)?,
        }
    }

    Some(Want {
        cid: cid?,
        want_type,
        cancel,
        send_dont_have,
    })
}

fn decode_block(input: &[u8], message: &mut Message) -> Option<()> {
    let mut reader = Reader::new(input);
    let mut prefix: Option<&[u8]> = None;
    let mut data: Option<&[u8]> = None;

    while let Some((field, wire)) = reader.tag()? {
        match (field, wire) {
            (1, 2) => prefix = Some(reader.length_delimited()?),
            (2, 2) => data = Some(reader.length_delimited()?),
            _ => reader.skip(wire)?,
        }
    }

    // The identifier is rebuilt from the data rather than taken from the
    // sender. This is the whole security property of a content address:
    // whatever bytes arrive, they are filed under the CID they hash to,
    // so a peer cannot make this node hold its bytes under a CID someone
    // else's records point at.
    if let (Some(prefix), Some(data)) = (prefix, data) {
        let mut binary = prefix.to_vec();
        binary.extend_from_slice(&openfiat_crypto::hash::sha256(data));
        if let Ok(cid) = Cid::from_binary(&binary) {
            message.blocks.push((cid, data.to_vec()));
        }
    }
    Some(())
}

fn decode_presence(input: &[u8], message: &mut Message) -> Option<()> {
    let mut reader = Reader::new(input);
    let mut cid = None;
    let mut presence = Presence::Have;

    while let Some((field, wire)) = reader.tag()? {
        match (field, wire) {
            (1, 2) => cid = Cid::from_binary(reader.length_delimited()?).ok(),
            (2, 0) => {
                presence = if reader.varint()? == 1 {
                    Presence::DontHave
                } else {
                    Presence::Have
                }
            }
            _ => reader.skip(wire)?,
        }
    }

    if let Some(cid) = cid {
        message.presences.push((cid, presence));
    }
    Some(())
}

/// A protobuf cursor that returns `None` rather than panicking on any
/// malformed input, since every byte it reads came from a stranger.
struct Reader<'a> {
    input: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(input: &'a [u8]) -> Self {
        Self { input }
    }

    /// The next `(field number, wire type)`, or `None` at the end.
    ///
    /// The double option distinguishes the two: `Some(None)` is a clean
    /// end of input, `None` is malformed.
    #[allow(clippy::type_complexity)]
    fn tag(&mut self) -> Option<Option<(u64, u8)>> {
        if self.input.is_empty() {
            return Some(None);
        }
        let key = self.varint()?;
        Some(Some((key >> 3, (key & 0x7) as u8)))
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value: u64 = 0;
        for index in 0..10 {
            let byte = *self.input.first()?;
            self.input = &self.input[1..];
            value |= u64::from(byte & 0x7f) << (index * 7);
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        // Ten continuation bytes would overflow a u64; a peer sending
        // them is not encoding a number this reader should keep chasing.
        None
    }

    fn length_delimited(&mut self) -> Option<&'a [u8]> {
        let length = usize::try_from(self.varint()?).ok()?;
        if length > self.input.len() {
            return None;
        }
        let (taken, rest) = self.input.split_at(length);
        self.input = rest;
        Some(taken)
    }

    /// Advances past a field this parser does not model.
    fn skip(&mut self, wire: u8) -> Option<()> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => self.take(8),
            2 => self.length_delimited().map(|_| ()),
            5 => self.take(4),
            // Wire types 3 and 4 are protobuf's removed group encoding,
            // and 6/7 have never existed. Skipping requires knowing how
            // long the field is, and for these there is no answer.
            _ => None,
        }
    }

    fn take(&mut self, count: usize) -> Option<()> {
        if self.input.len() < count {
            return None;
        }
        self.input = &self.input[count..];
        Some(())
    }
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn write_varint_field(out: &mut Vec<u8>, field: u64, value: u64) {
    write_varint(out, field << 3);
    write_varint(out, value);
}

fn write_bytes(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    write_varint(out, (field << 3) | 2);
    write_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

    #[test]
    fn a_wantlist_round_trips() {
        let message = Message {
            wants: vec![
                Want {
                    cid: fixtures::probe_cid(),
                    want_type: WantType::Block,
                    cancel: false,
                    send_dont_have: true,
                },
                Want {
                    cid: fixtures::other_cid(),
                    want_type: WantType::Have,
                    cancel: true,
                    send_dont_have: false,
                },
            ],
            ..Message::default()
        };
        assert_eq!(Message::decode(&message.encode()), Some(message));
    }

    #[test]
    fn a_block_round_trips_and_arrives_under_the_cid_of_its_own_bytes() {
        let message = Message {
            blocks: vec![(fixtures::probe_cid(), PROBE_CONTENT.to_vec())],
            ..Message::default()
        };
        let decoded = Message::decode(&message.encode()).unwrap();
        assert_eq!(decoded.blocks, message.blocks);
    }

    #[test]
    fn a_block_labelled_with_someone_elses_cid_arrives_under_its_own() {
        // The attack this defeats: a peer answers a want for CID A with
        // its own bytes, so the node stores them and later serves them to
        // a challenger — or to a browser — as the content A names. The
        // prefix is honoured, the digest is not: the block is re-addressed
        // from what actually arrived.
        let honest = fixtures::probe_cid();
        let mut forged = Vec::new();
        let mut block = Vec::new();
        write_bytes(&mut block, 1, &honest.prefix());
        write_bytes(&mut block, 2, b"substituted bytes");
        write_bytes(&mut forged, 3, &block);

        let decoded = Message::decode(&forged).unwrap();
        assert_eq!(decoded.blocks.len(), 1);
        assert_ne!(
            decoded.blocks[0].0, honest,
            "bytes must never inherit the CID they were sent under"
        );
        assert!(decoded.blocks[0].0.matches(b"substituted bytes"));
    }

    #[test]
    fn presences_round_trip_in_both_directions() {
        let message = Message {
            presences: vec![
                (fixtures::probe_cid(), Presence::Have),
                (fixtures::other_cid(), Presence::DontHave),
            ],
            ..Message::default()
        };
        assert_eq!(Message::decode(&message.encode()), Some(message));
    }

    #[test]
    fn a_field_this_parser_does_not_model_is_skipped_not_rejected() {
        // Field 5 (`pendingBytes`, a varint) and field 2 (bitswap 1.0's
        // bare block list) both appear from real implementations. A parser
        // that rejected them would refuse messages that are perfectly
        // valid, so forward compatibility is asserted rather than assumed.
        let mut input = Vec::new();
        write_varint_field(&mut input, 5, 4096);
        write_bytes(&mut input, 2, PROBE_CONTENT);
        let mut presence = Vec::new();
        write_bytes(&mut presence, 1, &fixtures::probe_cid().to_binary());
        write_bytes(&mut input, 4, &presence);

        let decoded = Message::decode(&input).expect("unknown fields must not fail the message");
        assert_eq!(
            decoded.presences,
            vec![(fixtures::probe_cid(), Presence::Have)]
        );
    }

    #[test]
    fn an_entry_naming_a_cid_this_protocol_cannot_hold_is_dropped_not_fatal() {
        // A blake3 CID: a legitimate thing for an IPFS peer to want, and
        // not something this node can ever verify. The rest of the
        // wantlist must still be answered.
        let mut blake3 = vec![0x01u8, 0x55, 0x1e, 0x20];
        blake3.extend_from_slice(&[7u8; 32]);

        let mut wantlist = Vec::new();
        for cid_bytes in [blake3, fixtures::probe_cid().to_binary()] {
            let mut entry = Vec::new();
            write_bytes(&mut entry, 1, &cid_bytes);
            write_bytes(&mut wantlist, 1, &entry);
        }
        let mut input = Vec::new();
        write_bytes(&mut input, 1, &wantlist);

        let decoded = Message::decode(&input).unwrap();
        assert_eq!(decoded.wants.len(), 1);
        assert_eq!(decoded.wants[0].cid, fixtures::probe_cid());
    }

    #[test]
    fn a_wantlist_longer_than_the_cap_is_truncated_rather_than_refused() {
        let mut wantlist = Vec::new();
        for _ in 0..MAX_WANTLIST_ENTRIES + 50 {
            let mut entry = Vec::new();
            write_bytes(&mut entry, 1, &fixtures::probe_cid().to_binary());
            write_bytes(&mut wantlist, 1, &entry);
        }
        let mut input = Vec::new();
        write_bytes(&mut input, 1, &wantlist);

        let decoded = Message::decode(&input).unwrap();
        assert_eq!(decoded.wants.len(), MAX_WANTLIST_ENTRIES);
    }

    #[test]
    fn malformed_input_is_refused_rather_than_partially_believed() {
        for hostile in [
            // A length prefix longer than the bytes that follow.
            vec![0x0a, 0xff, 0x01, 0x00],
            // Wire type 3: protobuf's removed group encoding, which
            // cannot be skipped because its length is unknowable.
            vec![0x0b, 0x00],
            // A varint that never terminates.
            vec![0xff; 12],
        ] {
            assert_eq!(Message::decode(&hostile), None, "{hostile:?}");
        }
    }

    #[test]
    fn an_empty_message_is_valid_and_empty() {
        let decoded = Message::decode(&[]).expect("an empty message is well-formed");
        assert!(decoded.is_empty());
    }
}
