//! Fetching a block from a public IPFS gateway.
//!
//! # Why a node that speaks bitswap still needs this
//!
//! Bitswap moves blocks between peers that have them. It does not create
//! the first copy. Attachments enter this network through a pinning
//! service — the interface's upload route holds the credential and hands
//! back a CID — so at the moment a record is published, the only holder
//! is that provider, which is not an OpenFiat peer and never will be.
//!
//! Without a path to the wider network, no OpenFiat node would ever hold
//! any content, every node would answer every wantlist with DontHave, and
//! the durability premium would be paid to nobody. This is that path, used
//! once per CID: fetch it, verify it, keep it — after which it is
//! available over bitswap from a node that is a peer.
//!
//! # The gateway is transport, not an authority
//!
//! It can serve the wrong bytes, no bytes, or log who asked for what. It
//! cannot change which content a CID names, because the CID is a hash of
//! that content. So the bytes are checked against the CID before they are
//! stored, and a gateway that substitutes anything fails that check and is
//! indistinguishable from one that is simply down.
//!
//! Privacy is the one thing verification does not fix: whoever runs the
//! gateway learns that this node asked for this CID. That is why an
//! operator can point at their own, and why a node prefers its peers —
//! this is the fallback, not the first choice.

use crate::held::MAX_HELD_BYTES;
use crate::pinning::PinError;
use openfiat_crypto::Cid;

/// Where a node fetches a block it cannot get from a peer.
///
/// Filebase's gateway, matching what the interface reads from, so a
/// node and a browser fetching the same attachment do not disagree about
/// where the network's content lives. An operator who would rather not
/// tell a third party what their node is fetching can point this
/// elsewhere, including at their own gateway.
pub const DEFAULT_GATEWAY: &str = "https://ipfs.filebase.io";

pub struct GatewayFetcher {
    endpoint: String,
    http: reqwest::Client,
}

impl GatewayFetcher {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// Retrieves the single block `cid` names.
    ///
    /// `?format=raw` asks for the block itself rather than the file a
    /// gateway would otherwise reconstruct — which matters, because what
    /// this node serves over bitswap is blocks. Requesting the assembled
    /// file and storing it under a block's CID would produce something
    /// that fails verification for every peer that checked.
    ///
    /// Only meaningful for a verifiable CID. A dag-pb root names a DAG
    /// this code cannot walk, so fetching one would produce bytes nothing
    /// could check — refused up front rather than stored unverified.
    pub async fn block(&self, cid: &Cid) -> Result<Vec<u8>, PinError> {
        if !cid.is_verifiable() {
            return Err(PinError::Unavailable(
                "a chunked CID names a DAG this node cannot verify".into(),
            ));
        }

        // `cid` is a validated `Cid` — base32 lowercase with no character
        // that could alter a path or query — which is the property
        // `openfiat_crypto::cid`'s parser exists to guarantee, and the
        // reason this is interpolated rather than escaped.
        let url = format!("{}/ipfs/{}?format=raw", self.endpoint, cid.as_str());
        let response = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.ipld.raw")
            .send()
            .await
            .map_err(|e| PinError::Unavailable(e.to_string()))?;

        if !response.status().is_success() {
            return Err(PinError::Unavailable(format!(
                "gateway returned {}",
                response.status()
            )));
        }

        // Streamed against a running cap rather than buffered whole. A
        // CID names content of unknown size until it arrives and
        // `Content-Length` is a claim by whoever is serving it, so the
        // only cap that holds is one applied as the bytes land.
        let mut body = Vec::new();
        let mut stream = response;
        while let Some(chunk) = stream
            .chunk()
            .await
            .map_err(|e| PinError::Unavailable(e.to_string()))?
        {
            if body.len() + chunk.len() > MAX_HELD_BYTES {
                return Err(PinError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        // The line that makes the gateway untrusted rather than believed.
        if !cid.matches(&body) {
            return Err(PinError::ContentMismatch);
        }
        Ok(body)
    }
}

impl Default for GatewayFetcher {
    fn default() -> Self {
        Self::new(DEFAULT_GATEWAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_slash_does_not_produce_a_double_slash_path() {
        assert_eq!(
            GatewayFetcher::new("https://example.com/").endpoint,
            "https://example.com"
        );
    }

    #[tokio::test]
    async fn a_chunked_cid_is_refused_before_any_request_is_made() {
        // Port 1 is reserved and nothing listens there, so a request
        // would fail with `Unavailable`. Getting the refusal instead
        // proves the check happened first — which matters, because the
        // failure mode being avoided is storing bytes nothing can check.
        let chunked =
            Cid::parse("bafybeig3ci7io2duyknu34co3zw42oodnfyamwazsus42vpgnvq2hpzodm").unwrap();
        let fetcher = GatewayFetcher::new("http://127.0.0.1:1");
        assert!(matches!(
            fetcher.block(&chunked).await,
            Err(PinError::Unavailable(detail)) if detail.contains("DAG")
        ));
    }

    /// The one test here that touches the real internet, so it is not in
    /// the default run — but it is the only one that proves the default
    /// gateway actually answers `?format=raw` with the block rather than
    /// with a reassembled file or an HTML page. Verified by hand at the
    /// time of writing: 31 bytes, sha256 `236435f9…`, matching the CID.
    ///
    /// `cargo test -p openfiat-content -- --ignored gateway`
    #[tokio::test]
    #[ignore = "reaches the public IPFS gateway"]
    async fn the_default_gateway_really_serves_the_block_this_cid_names() {
        let cid = crate::fixtures::probe_cid();
        let bytes = GatewayFetcher::default()
            .block(&cid)
            .await
            .expect("the probe CID is genuinely pinned and publicly retrievable");
        assert_eq!(bytes, b"openfiat ipfs probe 1785426891\n");
    }

    #[tokio::test]
    async fn an_unreachable_gateway_is_unavailable_rather_than_a_mismatch() {
        // The distinction matters: one means "try another gateway", the
        // other means the content served was not the content named.
        let fetcher = GatewayFetcher::new("http://127.0.0.1:1");
        assert!(matches!(
            fetcher.block(&crate::fixtures::probe_cid()).await,
            Err(PinError::Unavailable(_))
        ));
    }
}
