//! Answering a bitswap peer, and reading one.
//!
//! # The shape of the protocol, which is not request/response
//!
//! A bitswap message is fire-and-forget. A peer opens a stream, writes one
//! or more messages, and closes it; the answer comes back later over a
//! *new* stream opened in the other direction. That is why this is built
//! on `libp2p-stream` rather than on the workspace's existing
//! `request_response` behaviour — modelling a reply that arrives on a
//! different connection as a response to a request on this one would be
//! modelling something the protocol does not do, and would deadlock
//! against every real implementation.
//!
//! # Splitting the decision from the plumbing
//!
//! [`respond`] is a pure function: a message in, a message out, no I/O and
//! no networking. Everything a reviewer needs to check about what this
//! node discloses and to whom is decidable there, and its tests need no
//! sockets. The async functions around it move bytes and nothing else.

use super::message::{MAX_MESSAGE_BYTES, Message, Presence, WantType};
use crate::held::{HeldContent, MAX_BLOCK_BYTES};
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p_swarm::StreamProtocol;
use openfiat_crypto::Cid;
use openfiat_storage::KvStore;
use std::io;

/// The bitswap revision this node speaks.
///
/// 1.2.0 only. It is what Kubo, Helia and every gateway of consequence
/// negotiate, and it is the revision that added want-have and DontHave —
/// without which a peer asking this node for content it does not hold
/// waits for a timeout instead of being told. Supporting 1.1.0 as well
/// would add a second inbound stream to poll in exchange for reaching
/// implementations that have been superseded for years.
pub const PROTOCOL: StreamProtocol = StreamProtocol::new("/ipfs/bitswap/1.2.0");

/// The most this node will put in one reply.
///
/// A wantlist can name many blocks this node genuinely holds, and
/// answering all of them in one message would let a peer choose how much
/// memory this node assembles. Bitswap wantlists are resent, so a peer
/// whose request is only partly answered asks again and gets the rest —
/// truncating costs a round trip, not the content.
pub const MAX_RESPONSE_BYTES: usize = 4 * MAX_BLOCK_BYTES;

/// Somewhere blocks can be read from by CID.
///
/// A trait so [`respond`] can be tested against a handful of blocks in a
/// map rather than against a store, and so the node's own held content is
/// not the only thing that could ever back this.
pub trait BlockSource {
    fn block(&self, cid: &Cid) -> Option<Vec<u8>>;
}

impl<S: KvStore> BlockSource for HeldContent<S> {
    fn block(&self, cid: &Cid) -> Option<Vec<u8>> {
        self.get(cid)
    }
}

/// What this node sends back for `request`.
///
/// # What a peer can learn from the answer
///
/// Exactly one thing: whether this node holds a CID the asker already
/// knew. It cannot enumerate — there is no "list what you have" in
/// bitswap — so the answer discloses nothing the asker did not bring with
/// them. That is the reason this can serve strangers at all, and the
/// reason it is safe on by default.
///
/// Returns an empty message when there is nothing to say; the caller
/// should not open a stream for it. Silence is bitswap's ordinary answer
/// to a block one does not hold, and a peer that wants to be told
/// otherwise sets `sendDontHave` — honoured here, because it is what
/// turns a failed fetch into a fast failure rather than a timeout.
pub fn respond(blocks: &dyn BlockSource, request: &Message) -> Message {
    let mut reply = Message::default();
    let mut budget = MAX_RESPONSE_BYTES;

    for want in &request.wants {
        // A cancellation withdraws a want. This node answers each message
        // as it arrives and keeps no per-peer wantlist, so there is
        // nothing to withdraw from — but replying to one would be
        // answering a question the peer just took back.
        if want.cancel {
            continue;
        }

        match (blocks.block(&want.cid), want.want_type) {
            (Some(block), WantType::Block) => {
                if block.len() > budget {
                    // Out of budget for payload, but a `Have` still tells
                    // the peer to come back for it rather than to look
                    // elsewhere — a truthful answer that costs 40 bytes.
                    reply.presences.push((want.cid.clone(), Presence::Have));
                    continue;
                }
                budget -= block.len();
                reply.blocks.push((want.cid.clone(), block));
            }
            (Some(_), WantType::Have) => {
                reply.presences.push((want.cid.clone(), Presence::Have));
            }
            (None, _) if want.send_dont_have => {
                reply.presences.push((want.cid.clone(), Presence::DontHave));
            }
            (None, _) => {}
        }
    }

    reply
}

/// Reads one length-prefixed message, or `None` at a clean end of stream.
///
/// The length prefix arrives before the body, so it is checked against
/// [`MAX_MESSAGE_BYTES`] before anything is allocated. Without that, a
/// three-byte prefix claiming four gigabytes is a memory exhaustion
/// primitive available to any peer that can dial this node.
pub async fn read_message<T: AsyncRead + Unpin>(io: &mut T) -> io::Result<Option<Message>> {
    let Some(length) = read_varint(io).await? else {
        return Ok(None);
    };
    let length = usize::try_from(length).unwrap_or(usize::MAX);
    if length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bitswap message of {length} bytes exceeds {MAX_MESSAGE_BYTES}"),
        ));
    }

    let mut body = vec![0u8; length];
    io.read_exact(&mut body).await?;
    Message::decode(&body)
        .map(Some)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed bitswap message"))
}

/// Writes one length-prefixed message and closes the stream.
///
/// Closing is part of the protocol rather than tidiness: the receiver
/// reads until end of stream, so a sender that keeps it open leaves the
/// peer waiting for a message that is not coming.
pub async fn write_message<T: AsyncWrite + Unpin>(io: &mut T, message: &Message) -> io::Result<()> {
    let body = message.encode();
    let mut framed = Vec::with_capacity(body.len() + 8);
    write_varint(&mut framed, body.len() as u64);
    framed.extend_from_slice(&body);
    io.write_all(&framed).await?;
    io.close().await
}

/// Reads every message a peer sends before closing the stream.
///
/// Bounded by [`MAX_INBOUND_MESSAGES`] because a peer that never closes
/// would otherwise hold this loop for as long as it liked. Whatever was
/// read before the bound is returned rather than discarded — a peer being
/// excessive does not make its first message dishonest.
pub async fn read_all<T: AsyncRead + Unpin>(io: &mut T) -> Vec<Message> {
    let mut messages = Vec::new();
    while messages.len() < MAX_INBOUND_MESSAGES {
        match read_message(io).await {
            Ok(Some(message)) => messages.push(message),
            // A clean end of stream, or a peer that sent something
            // malformed. Neither is worth distinguishing to the caller:
            // what was read is what there is.
            Ok(None) | Err(_) => break,
        }
    }
    messages
}

/// How many messages one inbound stream may carry.
pub const MAX_INBOUND_MESSAGES: usize = 16;

/// Unsigned varint, as go's `msgio` frames every bitswap message.
///
/// `None` only for a clean end of stream before any byte of the prefix;
/// a stream that ends mid-prefix is truncated, which is an error.
async fn read_varint<T: AsyncRead + Unpin>(io: &mut T) -> io::Result<Option<u64>> {
    let mut value: u64 = 0;
    for index in 0..9 {
        let mut byte = [0u8; 1];
        match io.read(&mut byte).await {
            Ok(0) if index == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended inside a length prefix",
                ));
            }
            Ok(_) => {}
            Err(err) => return Err(err),
        }
        value |= u64::from(byte[0] & 0x7f) << (index * 7);
        if byte[0] & 0x80 == 0 {
            return Ok(Some(value));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "length prefix never terminated",
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitswap::message::Want;
    use crate::fixtures;
    use std::collections::HashMap;

    const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

    #[derive(Default)]
    struct Blocks(HashMap<String, Vec<u8>>);

    impl Blocks {
        fn holding(entries: &[(&Cid, &[u8])]) -> Self {
            Self(
                entries
                    .iter()
                    .map(|(cid, bytes)| (cid.as_str().to_string(), bytes.to_vec()))
                    .collect(),
            )
        }
    }

    impl BlockSource for Blocks {
        fn block(&self, cid: &Cid) -> Option<Vec<u8>> {
            self.0.get(cid.as_str()).cloned()
        }
    }

    fn want(cid: &Cid, want_type: WantType, send_dont_have: bool) -> Message {
        Message {
            wants: vec![Want {
                cid: cid.clone(),
                want_type,
                cancel: false,
                send_dont_have,
            }],
            ..Message::default()
        }
    }

    #[test]
    fn a_block_this_node_holds_is_served() {
        let blocks = Blocks::holding(&[(&fixtures::probe_cid(), PROBE_CONTENT)]);
        let reply = respond(
            &blocks,
            &want(&fixtures::probe_cid(), WantType::Block, false),
        );
        assert_eq!(
            reply.blocks,
            vec![(fixtures::probe_cid(), PROBE_CONTENT.to_vec())]
        );
        assert!(reply.presences.is_empty());
    }

    #[test]
    fn a_want_have_gets_an_answer_and_not_the_bytes() {
        // The distinction is the whole point of want-have: a peer sizing
        // up who to fetch from should not be sent the content by everyone
        // it asks.
        let blocks = Blocks::holding(&[(&fixtures::probe_cid(), PROBE_CONTENT)]);
        let reply = respond(
            &blocks,
            &want(&fixtures::probe_cid(), WantType::Have, false),
        );
        assert_eq!(
            reply.presences,
            vec![(fixtures::probe_cid(), Presence::Have)]
        );
        assert!(reply.blocks.is_empty());
    }

    #[test]
    fn content_this_node_does_not_hold_is_answered_only_if_asked_to_be() {
        let blocks = Blocks::default();

        let silent = respond(
            &blocks,
            &want(&fixtures::probe_cid(), WantType::Block, false),
        );
        assert!(silent.is_empty(), "bitswap's default answer is silence");

        let told = respond(
            &blocks,
            &want(&fixtures::probe_cid(), WantType::Block, true),
        );
        assert_eq!(
            told.presences,
            vec![(fixtures::probe_cid(), Presence::DontHave)]
        );
    }

    #[test]
    fn a_cancelled_want_is_not_answered() {
        let blocks = Blocks::holding(&[(&fixtures::probe_cid(), PROBE_CONTENT)]);
        let request = Message {
            wants: vec![Want {
                cid: fixtures::probe_cid(),
                want_type: WantType::Block,
                cancel: true,
                send_dont_have: true,
            }],
            ..Message::default()
        };
        assert!(respond(&blocks, &request).is_empty());
    }

    #[test]
    fn a_reply_past_the_budget_offers_the_rest_rather_than_sending_it() {
        // A peer must not be able to choose how much this node assembles
        // in one message. It is still told the content is here, so it
        // comes back rather than concluding this node has nothing.
        let big = vec![0u8; MAX_BLOCK_BYTES];
        let cid = {
            let mut binary = vec![0x01u8, 0x55, 0x12, 0x20];
            binary.extend_from_slice(&openfiat_crypto::hash::sha256(&big));
            Cid::from_binary(&binary).unwrap()
        };
        let blocks = Blocks::holding(&[(&cid, &big)]);
        let request = Message {
            wants: (0..6)
                .map(|_| Want {
                    cid: cid.clone(),
                    want_type: WantType::Block,
                    cancel: false,
                    send_dont_have: false,
                })
                .collect(),
            ..Message::default()
        };

        let reply = respond(&blocks, &request);
        let served: usize = reply.blocks.iter().map(|(_, b)| b.len()).sum();
        assert!(served <= MAX_RESPONSE_BYTES, "served {served} bytes");
        assert_eq!(reply.blocks.len(), 4);
        assert_eq!(reply.presences.len(), 2);
        assert!(reply.presences.iter().all(|(_, p)| *p == Presence::Have));
    }

    #[test]
    fn a_wantlist_naming_nothing_produces_no_stream_at_all() {
        assert!(respond(&Blocks::default(), &Message::default()).is_empty());
    }

    #[tokio::test]
    async fn a_message_survives_the_framing_it_is_written_in() {
        let message = Message {
            blocks: vec![(fixtures::probe_cid(), PROBE_CONTENT.to_vec())],
            ..Message::default()
        };
        let mut wire = Vec::new();
        write_message(&mut wire, &message).await.unwrap();

        let mut cursor = futures::io::Cursor::new(wire);
        assert_eq!(read_message(&mut cursor).await.unwrap(), Some(message));
        assert_eq!(
            read_message(&mut cursor).await.unwrap(),
            None,
            "a closed stream must read as ended, not as an error"
        );
    }

    #[tokio::test]
    async fn a_length_prefix_larger_than_the_cap_is_refused_before_allocating() {
        // The prefix claims 16 MiB and the body is four bytes. A reader
        // that trusted it would allocate first and discover the lie after.
        let mut wire = Vec::new();
        write_varint(&mut wire, 16 * 1024 * 1024);
        wire.extend_from_slice(b"tiny");

        let mut cursor = futures::io::Cursor::new(wire);
        let err = read_message(&mut cursor).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn a_stream_that_ends_inside_its_prefix_is_an_error_not_an_ending() {
        let mut cursor = futures::io::Cursor::new(vec![0x80u8]);
        let err = read_message(&mut cursor).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn reading_a_stream_stops_at_the_message_bound() {
        let mut wire = Vec::new();
        let message = Message {
            presences: vec![(fixtures::probe_cid(), Presence::Have)],
            ..Message::default()
        };
        let body = message.encode();
        for _ in 0..MAX_INBOUND_MESSAGES + 10 {
            write_varint(&mut wire, body.len() as u64);
            wire.extend_from_slice(&body);
        }

        let mut cursor = futures::io::Cursor::new(wire);
        assert_eq!(read_all(&mut cursor).await.len(), MAX_INBOUND_MESSAGES);
    }
}
