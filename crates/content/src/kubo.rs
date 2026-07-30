//! A [`PinningClient`] speaking Kubo's HTTP RPC.
//!
//! The API every IPFS implementation of consequence exposes: a local
//! `ipfs daemon`, a remote Kubo, or a hosted endpoint that mimics it.
//! Two calls are used, `pin/add` and `cat`, both POST — Kubo rejects GET
//! on its RPC surface, which is a deliberate CSRF defence on their side
//! and a common first thing to get wrong on ours.

use crate::pinning::{MAX_FETCH_BYTES, PinError, PinningClient};
use openfiat_crypto::Cid;

pub struct KuboClient {
    endpoint: String,
    http: reqwest::Client,
}

impl KuboClient {
    /// `endpoint` is the API root, e.g. `http://127.0.0.1:5001`.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait(?Send)]
impl PinningClient for KuboClient {
    async fn pin(&self, cid: &Cid) -> Result<(), PinError> {
        // `cid` is a validated `Cid`, so it is base32 lowercase with no
        // character that could alter the query string. That is the
        // property `openfiat_crypto::cid`'s parser exists to guarantee,
        // and the reason this can be interpolated rather than escaped.
        let url = format!("{}/api/v0/pin/add?arg={}", self.endpoint, cid.as_str());
        let response = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| PinError::Unavailable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(PinError::Unavailable(format!(
                "pin/add returned {}",
                response.status()
            )));
        }
        Ok(())
    }

    async fn fetch(&self, cid: &Cid) -> Result<Vec<u8>, PinError> {
        let url = format!("{}/api/v0/cat?arg={}", self.endpoint, cid.as_str());
        let response = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| PinError::Unavailable(e.to_string()))?;
        if !response.status().is_success() {
            return Err(PinError::Unavailable(format!(
                "cat returned {}",
                response.status()
            )));
        }

        // Streamed with a running cap rather than `bytes()`, which would
        // buffer the whole response first. A CID names content of unknown
        // size until it arrives and `Content-Length` is a claim by
        // whatever is serving it, so the only cap that holds is one
        // applied as the bytes land.
        let mut body = Vec::new();
        let mut stream = response;
        while let Some(chunk) = stream
            .chunk()
            .await
            .map_err(|e| PinError::Unavailable(e.to_string()))?
        {
            if body.len() + chunk.len() > MAX_FETCH_BYTES {
                return Err(PinError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        // Checked even though this is our own daemon. Serving unverified
        // bytes to a challenger turns one bad local read into what looks,
        // from outside, like this node lying.
        if !cid.matches(&body) {
            return Err(PinError::ContentMismatch);
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_slash_does_not_produce_a_double_slash_path() {
        let client = KuboClient::new("http://127.0.0.1:5001/");
        assert_eq!(client.endpoint, "http://127.0.0.1:5001");
    }

    #[tokio::test]
    async fn an_unreachable_daemon_is_unavailable_rather_than_a_mismatch() {
        // Port 1 is reserved and nothing listens there. The distinction
        // matters: "your daemon is down" is an operator problem, while a
        // mismatch would suggest the content itself is wrong.
        let client = KuboClient::new("http://127.0.0.1:1");
        assert!(matches!(
            client.fetch(&crate::fixtures::probe_cid()).await,
            Err(PinError::Unavailable(_))
        ));
    }
}
