//! Content over plain HTTP: `GET /ipfs/{cid}`, so a browser can put an
//! attachment or an avatar in an `<img>` tag.
//!
//! # Why the node has to serve this itself now
//!
//! An interface used to fetch attachments through a public IPFS gateway,
//! which found a provider through the public DHT. This node no longer
//! publishes there — its content offering is private (see
//! [`openfiat_network::content_routing`]) — so no public gateway can
//! resolve an OpenFiat CID any more, and every image in the interface
//! would be broken if the only content path were one no browser speaks.
//!
//! The path shape is deliberate. `https://<node>/ipfs/<cid>` is the same
//! URL a public gateway answers, so an interface configured with a
//! gateway base URL points it at an OpenFiat node and changes nothing
//! else.
//!
//! # This is the unverified read, and it is bounded accordingly
//!
//! A browser checks nothing. It cannot: a CID above one block names a
//! dag-pb root whose digest covers the root node rather than the file, so
//! there is no check to perform even in principle, and that is equally
//! true of every public gateway. What that costs is stated plainly in
//! [`crate::methods::attachments`]; what it buys is that the trust lands
//! on the node the client already selected rather than on a stranger.
//!
//! So the surface is kept as small as the job requires:
//!
//! - **No sniffing.** The response type is decided by
//!   [`MediaType::looks_like`] against the file's own magic bytes — the
//!   same four types the upload path accepts — and everything else is
//!   `application/octet-stream`. A caller cannot get this node to label
//!   arbitrary bytes as anything a browser will execute.
//! - **Nothing renders in this origin.** `Content-Security-Policy:
//!   default-src 'none'; sandbox` and `X-Content-Type-Options: nosniff`
//!   go on every response, and anything not a recognised image is
//!   additionally sent as an attachment. Public gateways solve this by
//!   putting each CID on its own subdomain; a node has one hostname, so
//!   the headers do the work instead. Without them, uploading an HTML
//!   file would be stored XSS against the node's own origin — and the
//!   ingress is open by design.
//! - **Immutable caching.** A CID names its bytes, so the answer can
//!   never change. `max-age=31536000, immutable` is honest here in a way
//!   it almost never is elsewhere.

use crate::actor::RpcHandle;
use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use openfiat_content::MediaType;

/// The content routes, to be merged into the node's main router.
///
/// Both paths answer identically. `/ipfs/{cid}` is the compatibility
/// spelling every gateway client already builds; `/content/{cid}` is the
/// name that is true — these bytes come from this node's own store, not
/// from IPFS — and is what this project's own documentation uses.
pub fn router(rpc: RpcHandle) -> Router {
    Router::new()
        .route("/ipfs/{cid}", get(handle_content))
        .route("/content/{cid}", get(handle_content))
        .with_state(rpc)
}

/// The path a CID is served under, relative to the node's HTTP root.
/// Shared so a URL handed to a client and the route that answers it
/// cannot drift apart.
pub fn content_path(cid: &str) -> String {
    format!("ipfs/{cid}")
}

async fn handle_content(State(rpc): State<RpcHandle>, AxumPath(cid): AxumPath<String>) -> Response {
    // Straight through the actor, which is where the blocks are. A whole
    // file is bounded by `openfiat_content::dag::MAX_DAG_BYTES` — the
    // largest an attachment may be — so this cannot become an unbounded
    // read on the node's single-threaded event loop.
    let response = rpc
        .call("getContentFile", serde_json::json!({ "cid": cid.as_str() }))
        .await;

    let file = match response {
        Ok(value) => match value.get("content").and_then(|c| c.as_str()) {
            Some(encoded) => match BASE64.decode(encoded.as_bytes()) {
                Ok(bytes) => bytes,
                Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "undecodable content"),
            },
            // `null`: this node holds no copy. A 404 rather than a 502 —
            // the request was well-formed and the honest answer is that
            // the content is not here.
            None => {
                return error(
                    StatusCode::NOT_FOUND,
                    "this node does not hold that content",
                );
            }
        },
        // An unparseable CID, or a DAG this node cannot serve as a file —
        // the caller's fault. Anything else is this node's.
        Err(crate::error::RpcError::InvalidParams(reason)) => {
            return error(StatusCode::BAD_REQUEST, &reason);
        }
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "content unavailable"),
    };

    let media_type = recognise(&file);
    let mut response = (
        [(
            header::CONTENT_TYPE,
            media_type.map_or("application/octet-stream", MediaType::as_str),
        )],
        file,
    )
        .into_response();

    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    // A PDF is a recognised type and still a scripting host, so only the
    // three bitmap formats are served for display. Everything else
    // downloads.
    if !media_type.is_some_and(MediaType::is_image) {
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
    }
    response
}

/// The media type this file genuinely is, by its own leading bytes.
///
/// `None` is not a failure — it is content outside the four types this
/// protocol renders, which is served as opaque bytes rather than guessed
/// at.
fn recognise(file: &[u8]) -> Option<MediaType> {
    [
        MediaType::Png,
        MediaType::Jpeg,
        MediaType::Webp,
        MediaType::Pdf,
    ]
    .into_iter()
    .find(|candidate| candidate.looks_like(file))
}

fn error(status: StatusCode, detail: &str) -> Response {
    (
        status,
        [(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; sandbox",
        )],
        detail.to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::{NetworkConfig, spawn_actor};
    use http_body_util::BodyExt;
    use openfiat_crypto::Cid;
    use openfiat_storage::mem::MemoryStore;
    use tower::ServiceExt;

    /// A real 1×1 PNG, so the media-type check is decided by bytes a
    /// decoder would accept rather than by a magic number this test made
    /// up to agree with the one the code looks for.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn raw_cid(bytes: &[u8]) -> Cid {
        let mut binary = vec![0x01, 0x55, 0x12, 0x20];
        binary.extend_from_slice(&openfiat_crypto::hash::sha256(bytes));
        Cid::from_binary(&binary).unwrap()
    }

    fn dag_cid(bytes: &[u8]) -> Cid {
        let mut binary = vec![0x01, 0x70, 0x12, 0x20];
        binary.extend_from_slice(&openfiat_crypto::hash::sha256(bytes));
        Cid::from_binary(&binary).unwrap()
    }

    /// A dag-pb node linking to `children`, written as the literal wire
    /// bytes — see `openfiat_content::dag`'s own fixture for why.
    fn dag_node(children: &[&Cid]) -> Vec<u8> {
        let mut out = Vec::new();
        for child in children {
            let hash = child.to_binary();
            let mut link = vec![0x0a, hash.len() as u8];
            link.extend_from_slice(&hash);
            link.extend_from_slice(&[0x12, 0x00]);
            out.push(0x12);
            out.push(link.len() as u8);
            out.extend_from_slice(&link);
        }
        out.extend_from_slice(&[0x0a, 0x02, 0x08, 0x02]);
        out
    }

    /// A node holding `blocks`, reachable over its real HTTP surface.
    /// Uploaded through `sendContentPut` rather than poked into the store,
    /// so what is served is what an interface could actually have put
    /// there.
    async fn node_holding(blocks: &[(&Cid, &[u8])]) -> Router {
        let router = crate::server::router(
            spawn_actor(MemoryStore::new, NetworkConfig::for_test()),
            std::sync::Arc::new(openfiat_metrics::MetricsRegistry::new()),
            std::env::temp_dir().join("openfiat-gateway-test-snapshots"),
        );
        for (cid, bytes) in blocks {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "sendContentPut",
                "params": { "cid": cid.as_str(), "content": BASE64.encode(bytes) },
            });
            let response = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri("/rpc")
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        router
    }

    async fn get(router: &Router, uri: &str) -> Response {
        router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn body_of(response: Response) -> Vec<u8> {
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    #[tokio::test]
    async fn an_image_uploaded_to_this_node_is_served_from_this_node() {
        let cid = raw_cid(PNG);
        let router = node_holding(&[(&cid, PNG)]).await;

        let response = get(&router, &format!("/ipfs/{}", cid.as_str())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert!(
            !response.headers().contains_key(header::CONTENT_DISPOSITION),
            "an image must render rather than download"
        );
        assert_eq!(body_of(response).await, PNG);
    }

    /// The path shape is the compatibility promise: an interface that
    /// built `{gateway}/ipfs/{cid}` against a public gateway needs its
    /// base URL changed and nothing else.
    #[tokio::test]
    async fn the_ipfs_and_content_paths_answer_the_same_thing() {
        let cid = raw_cid(PNG);
        let router = node_holding(&[(&cid, PNG)]).await;

        let by_ipfs = get(&router, &format!("/ipfs/{}", cid.as_str())).await;
        let by_content = get(&router, &format!("/content/{}", cid.as_str())).await;
        assert_eq!(by_ipfs.status(), by_content.status());
        assert_eq!(body_of(by_ipfs).await, body_of(by_content).await);
    }

    /// The reason this route exists at all. Above 256 KiB a CID names a
    /// dag-pb root, and a browser fetching one gets an image or gets
    /// nothing — it cannot walk links.
    #[tokio::test]
    async fn a_chunked_file_is_served_as_one_file() {
        let first = raw_cid(b"the first chunk of ");
        let second = raw_cid(b"a file too large for one block");
        let root_block = dag_node(&[&first, &second]);
        let root = dag_cid(&root_block);

        let router = node_holding(&[
            (&first, b"the first chunk of "),
            (&second, b"a file too large for one block"),
            (&root, &root_block),
        ])
        .await;

        let response = get(&router, &format!("/ipfs/{}", root.as_str())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_of(response).await,
            b"the first chunk of a file too large for one block"
        );
    }

    /// A file missing a chunk is not a shorter file. Serving what is
    /// present would hand a browser a truncated image that looks like a
    /// successful fetch.
    #[tokio::test]
    async fn a_file_whose_chunks_are_not_all_here_is_not_served_in_part() {
        let present = raw_cid(b"the first chunk of ");
        let absent = raw_cid(b"a chunk this node never received");
        let root_block = dag_node(&[&present, &absent]);
        let root = dag_cid(&root_block);

        let router =
            node_holding(&[(&present, b"the first chunk of "), (&root, &root_block)]).await;

        let response = get(&router, &format!("/ipfs/{}", root.as_str())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn content_this_node_does_not_hold_is_a_404_and_not_an_error() {
        let router = node_holding(&[]).await;
        let response = get(&router, &format!("/ipfs/{}", raw_cid(PNG).as_str())).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_path_that_is_not_a_content_address_is_refused() {
        let router = node_holding(&[]).await;
        for bad in ["not-a-cid", "..", "%2e%2e%2fetc%2fpasswd"] {
            let response = get(&router, &format!("/ipfs/{bad}")).await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "{bad} was treated as a content address"
            );
        }
    }

    /// The security property of serving arbitrary uploads from the node's
    /// own hostname. The ingress is open by design, so without this a
    /// stranger uploads a page and this node hosts it — same origin as
    /// the JSON-RPC every wallet-signed request goes to.
    #[tokio::test]
    async fn an_uploaded_web_page_is_never_served_as_one() {
        let page = b"<html><script>fetch('/rpc')</script></html>";
        let cid = raw_cid(page);
        let router = node_holding(&[(&cid, page)]).await;

        let response = get(&router, &format!("/ipfs/{}", cid.as_str())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/octet-stream"
        );
        assert_eq!(
            response.headers()[header::CONTENT_DISPOSITION],
            "attachment"
        );
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'; sandbox"
        );
    }

    /// A PDF is recognised — so it is labelled honestly — and still
    /// downloads, because a PDF viewer is a scripting host.
    #[tokio::test]
    async fn a_pdf_is_labelled_honestly_and_still_downloads() {
        let pdf = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n";
        let cid = raw_cid(pdf);
        let router = node_holding(&[(&cid, pdf)]).await;

        let response = get(&router, &format!("/ipfs/{}", cid.as_str())).await;
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/pdf");
        assert_eq!(
            response.headers()[header::CONTENT_DISPOSITION],
            "attachment"
        );
    }
}
