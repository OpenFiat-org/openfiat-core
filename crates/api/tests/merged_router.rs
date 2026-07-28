//! Proves the actual intended composition: `openfiat_api::router()` merged
//! onto `openfiat_rpc::router(...)` serves both the live JSON-RPC surface
//! and its own documentation from one axum app — the shape a real node's
//! composition root (`openfiat-cli`) would build.

use http_body_util::BodyExt;
use openfiat_storage::mem::MemoryStore;
use tower::ServiceExt;

fn merged_router() -> axum::Router {
    let rpc_handle =
        openfiat_rpc::spawn_actor(MemoryStore::new, openfiat_rpc::NetworkConfig::for_test());
    let metrics = std::sync::Arc::new(openfiat_metrics::MetricsRegistry::new());
    openfiat_rpc::router(rpc_handle, metrics).merge(openfiat_api::router())
}

#[tokio::test]
async fn the_merged_router_serves_both_rpc_and_documentation() {
    let router = merged_router();

    let rpc_response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/rpc")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "getVersion", "params": {} }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rpc_response.status(), axum::http::StatusCode::OK);

    let docs_response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/openrpc.json")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(docs_response.status(), axum::http::StatusCode::OK);
    let body = docs_response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        document["methods"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "getVersion")
    );

    let reference_response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/docs")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reference_response.status(), axum::http::StatusCode::OK);
}
