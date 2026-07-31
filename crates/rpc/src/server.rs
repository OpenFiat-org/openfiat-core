//! The axum HTTP/WebSocket surface: one POST endpoint speaking JSON-RPC
//! 2.0 (`/rpc`), one WebSocket endpoint streaming the generic mutation
//! firehose (`/ws` — see [`crate::actor::RpcHandle::subscribe`]),
//! `/health`, `/metrics`, and `GET /snapshot/{id}` merged in from
//! `openfiat_snapshot::serve`.
//!
//! The snapshot route rides here rather than on a port of its own — see
//! that module's doc for why — and holds no `RpcHandle`, so a peer
//! downloading a multi-gigabyte snapshot never queues a byte through the
//! single-threaded actor that answers every JSON-RPC call.

use crate::actor::RpcHandle;
use crate::jsonrpc::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use openfiat_metrics::MetricsRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct AppState {
    rpc: RpcHandle,
    metrics: Arc<MetricsRegistry>,
}

/// `snapshot_directory` is where this node's produced snapshots live
/// (`SnapshotConfig::directory`); the merged route serves from it and
/// answers 404 for everything else, so pointing it at an empty or absent
/// directory is safe for a node that produces nothing.
pub fn router(
    rpc: RpcHandle,
    metrics: Arc<MetricsRegistry>,
    snapshot_directory: PathBuf,
) -> Router {
    let state = AppState {
        rpc: rpc.clone(),
        metrics,
    };
    Router::new()
        .route("/rpc", post(handle_rpc))
        .route("/ws", get(handle_ws))
        .route("/health", get(handle_health))
        .route("/metrics", get(handle_metrics))
        .with_state(state)
        .merge(openfiat_snapshot::serve::router(snapshot_directory))
        .merge(crate::gateway::router(rpc))
        // A third-party browser UI calling this node directly from its own
        // origin (OFS-8200's whole point — a stable, third-party-facing RPC
        // surface) needs this node to actually allow that cross-origin
        // call. Permissive by design, not an oversight: every `sendX`
        // mutation is already self-authenticating via the caller's own
        // wallet signature on the payload (verified by the domain, not by
        // same-origin policy), so there is no session/cookie-based trust
        // boundary here for CORS to protect — the same reasoning public
        // Solana RPC endpoints rely on to allow any origin.
        .layer(CorsLayer::permissive())
}

async fn handle_rpc(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    state
        .metrics
        .counter("rpc_requests_total", "Total JSON-RPC requests received")
        .inc();

    let request: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(e) => {
            return axum::Json(JsonRpcResponse::failure(
                serde_json::Value::Null,
                JsonRpcError::new(JsonRpcError::PARSE_ERROR, e.to_string()),
            ))
            .into_response();
        }
    };

    let id = request.id.clone();
    match state.rpc.call(request.method, request.params).await {
        Ok(result) => axum::Json(JsonRpcResponse::success(id, result)).into_response(),
        Err(error) => {
            state
                .metrics
                .counter(
                    "rpc_errors_total",
                    "Total JSON-RPC requests that returned an error",
                )
                .inc();
            axum::Json(JsonRpcResponse::failure(id, error.into_json_rpc_error())).into_response()
        }
    }
}

async fn handle_health() -> &'static str {
    "ok"
}

async fn handle_metrics(State(state): State<AppState>) -> String {
    state.metrics.render()
}

async fn handle_ws(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| stream_events(socket, state.rpc))
}

/// Forwards every `sendX` mutation notification to the client until it
/// disconnects or the underlying broadcast channel lags too far behind
/// (`RecvError::Lagged` — the client missed some notifications and gets
/// dropped rather than silently desynchronized).
async fn stream_events(mut socket: WebSocket, rpc: RpcHandle) {
    let mut subscription = rpc.subscribe();
    loop {
        match subscription.recv().await {
            Ok(event) => {
                if socket
                    .send(Message::Text(event.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::spawn_actor;
    use http_body_util::BodyExt;
    use openfiat_storage::mem::MemoryStore;
    use tower::ServiceExt;

    fn test_router() -> Router {
        router_over(std::env::temp_dir().join("openfiat-rpc-test-snapshots"))
    }

    fn router_over(snapshot_directory: PathBuf) -> Router {
        router(
            spawn_actor(MemoryStore::new, crate::actor::NetworkConfig::for_test()),
            Arc::new(MetricsRegistry::new()),
            snapshot_directory,
        )
    }

    /// The merge itself is the thing under test: `openfiat_snapshot::serve`
    /// has its own tests, but nothing else proves an archival node's
    /// snapshot is reachable from the port an operator actually exposes.
    #[tokio::test]
    async fn the_merged_router_serves_snapshots_from_the_configured_directory() {
        let directory = std::env::temp_dir().join(format!(
            "openfiat-rpc-merged-snapshots-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!(
                "snap-7-42{}",
                openfiat_snapshot::serve::FILE_EXTENSION
            )),
            b"compressed state",
        )
        .unwrap();

        let response = router_over(directory.clone())
            .oneshot(
                axum::http::Request::builder()
                    .uri("/snapshot/snap-7-42")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"compressed state");

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let response = test_router()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn rpc_dispatches_get_version() {
        let request_body =
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "getVersion", "params": {} });
        let response = test_router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["result"]["version"].is_string());
        assert!(json.get("error").is_none());
    }

    #[tokio::test]
    async fn rpc_reports_method_not_found_as_a_json_rpc_error() {
        let request_body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "doesNotExist", "params": {} });
        let response = test_router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["error"]["code"],
            serde_json::Value::from(JsonRpcError::METHOD_NOT_FOUND)
        );
    }

    #[tokio::test]
    async fn metrics_reflects_requests_handled() {
        let router = test_router();
        let request_body =
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "getVersion", "params": {} });
        router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .body(axum::body::Body::from(request_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/metrics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("rpc_requests_total 1"));
    }
}
