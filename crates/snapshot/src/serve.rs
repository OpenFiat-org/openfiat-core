//! The archival half of OFS-1300 §14: an HTTP endpoint streaming a
//! produced snapshot's compressed bytes, `GET /snapshot/{id}`.
//!
//! **Why this rides the node's existing HTTP server rather than a port of
//! its own.** The bytes here are public and self-verifying (see
//! [`crate::location`]) — there is no confidentiality boundary a separate
//! port could enforce, so a second listener would buy nothing and cost an
//! operator a second bind address to configure, a second firewall rule to
//! open, a second thing to reverse-proxy, and a second graceful-shutdown
//! path. Merging into `openfiat-rpc`'s router instead means a snapshot is
//! reachable from the same `CLI_HTTP_ADDR` an operator already exposes,
//! under the permissive CORS layer that is already there, so a browser
//! client can fetch one as easily as it calls `getLatestSnapshot`.
//!
//! This router deliberately holds no node state: it reads files out of
//! the configured directory and nothing else. That keeps a multi-gigabyte
//! download off the single-threaded actor channel that carries every
//! JSON-RPC call — one peer bootstrapping must not be able to stall the
//! node's event loop — and it means the whole serving path is reviewable
//! without reasoning about `NodeState` at all.

use crate::record::SnapshotId;
use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::path::PathBuf;

/// The extension every produced snapshot file carries, and the only one
/// this router will serve.
pub const FILE_EXTENSION: &str = ".snapshot";

/// The path a snapshot is served under, relative to the node's HTTP root.
/// Shared with [`crate::producer`] so an announced URL and the route that
/// answers it cannot drift apart.
pub fn download_path(id: &SnapshotId) -> String {
    format!("snapshot/{}", id.as_str())
}

/// The snapshot-serving routes, to be merged into the node's main router.
pub fn router(directory: PathBuf) -> Router {
    Router::new()
        .route("/snapshot/{id}", get(handle_download))
        .with_state(directory)
}

/// Whether `id` is safe to turn into a filename.
///
/// This is the security control on the whole module: `id` arrives from an
/// unauthenticated caller and becomes a path. An allow-list of characters
/// with no `/`, no `\`, and no `.` run is checked *before* any path is
/// built, rather than trying to detect traversal after the fact — the
/// same reason a parser is preferred to a sanitizer everywhere else.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// Streams the requested snapshot from disk.
///
/// Streamed rather than buffered because a snapshot is the node's entire
/// state: reading one fully into memory per concurrent request is how an
/// archival node runs itself out of memory serving the very peers it
/// exists for.
async fn handle_download(
    State(directory): State<PathBuf>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    if !is_safe_id(&id) {
        return (StatusCode::BAD_REQUEST, "invalid snapshot id").into_response();
    }
    let path = directory.join(format!("{id}{FILE_EXTENSION}"));
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return (StatusCode::NOT_FOUND, "no such snapshot").into_response();
    };
    // Advertising the length lets a fetching node reject an
    // obviously-wrong body before downloading it — see
    // `crate::fetch::download`.
    let length = match file.metadata().await {
        Ok(metadata) => metadata.len(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "unreadable").into_response(),
    };

    let stream = tokio_util::io::ReaderStream::new(file);
    (
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
            (
                axum::http::header::CONTENT_LENGTH,
                &length.to_string() as &str,
            ),
        ],
        axum::body::Body::from_stream(stream),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn directory_with(id: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "openfiat-serve-{id}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(format!("{id}{FILE_EXTENSION}")), bytes).unwrap();
        path
    }

    async fn get(directory: PathBuf, uri: &str) -> (StatusCode, Vec<u8>) {
        let response = router(directory)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, body.to_vec())
    }

    #[tokio::test]
    async fn serves_a_snapshot_byte_for_byte() {
        let directory = directory_with("snap-1-9", b"compressed state bytes");
        let (status, body) = get(directory.clone(), "/snapshot/snap-1-9").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"compressed state bytes");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn an_unknown_snapshot_is_a_404_not_an_error() {
        let directory = directory_with("snap-1-9", b"x");
        let (status, _) = get(directory.clone(), "/snapshot/snap-does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The id becomes a filename, so this is the test that matters most
    /// in this module.
    #[tokio::test]
    async fn ids_that_could_escape_the_directory_are_refused() {
        for id in [
            "..%2F..%2Fetc%2Fpasswd",
            "..",
            "a..b",
            "snap%2F..%2Fsecret",
            "snap.1",
            "",
        ] {
            assert!(
                !is_safe_id(&percent_decode(id)),
                "{id} must not be accepted as a snapshot id"
            );
        }
        assert!(is_safe_id("snap-4217-1785263830"));
    }

    /// Axum decodes `%2F` before the handler sees it, so the guard has to
    /// be checked against the decoded form — which is what the route
    /// actually receives.
    fn percent_decode(raw: &str) -> String {
        raw.replace("%2F", "/").replace("%2E", ".")
    }

    #[tokio::test]
    async fn a_traversal_attempt_over_the_real_route_is_rejected() {
        let directory = directory_with("snap-2-9", b"x");
        let (status, _) = get(directory.clone(), "/snapshot/..%2F..%2Fetc%2Fpasswd").await;
        assert_ne!(status, StatusCode::OK);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
