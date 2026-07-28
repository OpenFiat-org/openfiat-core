//! The OFNP §13-16 message envelope and its wire codec.
//!
//! Every field OFNP §14 requires in the header is present. Full protocol
//! negotiation (§10 — comparing supported OFS specification lists) isn't
//! implemented yet: libp2p's own multistream-select already refuses to
//! open a stream for a protocol string neither side advertises, which
//! covers Phase 2's exit criteria. Per-OFS-spec version negotiation lands
//! once a crate above this one actually needs to declare which specs it
//! supports.

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::StreamProtocol;
use libp2p::request_response::Codec;
use openfiat_serialization::wire;
use openfiat_types::{Signature, Timestamp};
use std::io;

/// The stream protocol OpenFiat nodes negotiate for envelope exchange.
pub const PROTOCOL: StreamProtocol = StreamProtocol::new("/openfiat/envelope/1.0.0");

/// OFNP is currently at major version 1 (draft).
pub const PROTOCOL_VERSION: u16 = 1;

/// An envelope larger than this is rejected outright (OFNP §26: resource
/// exhaustion is an explicit threat this protocol must consider).
pub const MAX_ENVELOPE_BYTES: u32 = 1 << 20;

/// The standard header fields every OFNP message carries (§14).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Header {
    pub protocol_version: u16,
    /// The OFS specification number defining `message_type`'s payload
    /// shape (e.g. `1000` for a bare OFNP-level message like a heartbeat).
    pub ofs_spec: u16,
    pub message_type: String,
    pub sequence: u64,
    pub timestamp: Timestamp,
    pub payload_length: u32,
    pub compressed: bool,
    pub authenticated: bool,
}

/// A complete OFNP message: header, payload, and an optional signature
/// (§16 — required when `header.authenticated` is set).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Envelope {
    pub header: Header,
    pub payload: Vec<u8>,
    pub authentication: Option<Signature>,
}

impl Envelope {
    /// Build an unauthenticated envelope. `header.payload_length` and
    /// `header.compressed` are derived, not caller-supplied, so they can
    /// never disagree with the actual payload.
    pub fn new(
        ofs_spec: u16,
        message_type: impl Into<String>,
        sequence: u64,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            header: Header {
                protocol_version: PROTOCOL_VERSION,
                ofs_spec,
                message_type: message_type.into(),
                sequence,
                timestamp: Timestamp::now(),
                payload_length: payload.len() as u32,
                compressed: false,
                authenticated: false,
            },
            payload,
            authentication: None,
        }
    }

    /// Attach a signature over this envelope's payload, marking it authenticated.
    pub fn authenticated_with(mut self, signature: Signature) -> Self {
        self.header.authenticated = true;
        self.authentication = Some(signature);
        self
    }
}

/// Reads/writes [`Envelope`]s as a 4-byte big-endian length prefix followed
/// by its `postcard`-encoded bytes.
#[derive(Debug, Clone, Default)]
pub struct EnvelopeCodec;

async fn read_envelope<T: AsyncRead + Unpin + Send>(io: &mut T) -> io::Result<Envelope> {
    let mut len_bytes = [0u8; 4];
    io.read_exact(&mut len_bytes).await?;
    let len = u32::from_be_bytes(len_bytes);
    if len > MAX_ENVELOPE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "envelope exceeds maximum size",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    io.read_exact(&mut buf).await?;
    wire::from_bytes(&buf).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

async fn write_envelope<T: AsyncWrite + Unpin + Send>(
    io: &mut T,
    envelope: &Envelope,
) -> io::Result<()> {
    let bytes =
        wire::to_bytes(envelope).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    if bytes.len() > MAX_ENVELOPE_BYTES as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "envelope exceeds maximum size",
        ));
    }
    io.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    io.write_all(&bytes).await?;
    io.flush().await
}

#[async_trait]
impl Codec for EnvelopeCodec {
    type Protocol = StreamProtocol;
    type Request = Envelope;
    type Response = Envelope;

    async fn read_request<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Envelope>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_envelope(io).await
    }

    async fn read_response<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Envelope>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_envelope(io).await
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Envelope,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_envelope(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        res: Envelope,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_envelope(io, &res).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::io::Cursor;

    #[test]
    fn new_derives_payload_length_from_the_actual_payload() {
        let envelope = Envelope::new(1000, "Heartbeat", 1, vec![1, 2, 3]);
        assert_eq!(envelope.header.payload_length, 3);
        assert!(!envelope.header.authenticated);
        assert!(envelope.authentication.is_none());
    }

    #[test]
    fn authenticated_with_sets_the_flag_and_signature() {
        let signature = Signature::from_bytes([7u8; 64]);
        let envelope =
            Envelope::new(1000, "Heartbeat", 1, vec![]).authenticated_with(signature.clone());
        assert!(envelope.header.authenticated);
        assert_eq!(envelope.authentication, Some(signature));
    }

    #[tokio::test]
    async fn round_trips_an_envelope_through_the_wire_codec() {
        let envelope = Envelope::new(1200, "GossipEvent", 42, vec![9, 9, 9]);
        let mut buf = Vec::new();
        write_envelope(&mut buf, &envelope).await.unwrap();

        let mut cursor = Cursor::new(buf);
        let decoded = read_envelope(&mut cursor).await.unwrap();
        assert_eq!(decoded, envelope);
    }

    #[tokio::test]
    async fn rejects_a_length_prefix_over_the_maximum() {
        let mut buf = (MAX_ENVELOPE_BYTES + 1).to_be_bytes().to_vec();
        buf.extend_from_slice(&[0u8; 8]); // some bytes that would never be read
        let mut cursor = Cursor::new(buf);
        let err = read_envelope(&mut cursor).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
