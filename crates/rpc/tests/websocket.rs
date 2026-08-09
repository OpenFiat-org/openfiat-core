//! Phase 7's "at least one working WebSocket subscription end-to-end"
//! exit criterion, proven against a real bound socket (not just an
//! in-process `tower::oneshot` request) — a client connects to `/ws`,
//! a mutation is submitted through the same `RpcHandle` the server
//! holds, and the client receives the notification over the wire.

use futures_util::StreamExt;
use openfiat_crypto::Keypair;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_rpc::{NetworkConfig, router, spawn_actor};
use openfiat_sessions::events::{SessionCreate, SignedSessionCreate};
use openfiat_storage::mem::MemoryStore;
use openfiat_types::Timestamp;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn a_websocket_client_receives_a_notification_for_a_real_mutation() {
    let handle = spawn_actor(MemoryStore::new, NetworkConfig::for_test());
    let app = router(
        handle.clone(),
        Arc::new(openfiat_metrics::MetricsRegistry::new()),
        std::env::temp_dir().join("openfiat-websocket-test-snapshots"),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // `/ws` is behind the per-IP rate limiter (F-02), which keys on
        // the real socket peer via `ConnectInfo` -- only populated when
        // served through `into_make_service_with_connect_info`, matching
        // how `openfiat-cli` serves this router for real.
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("failed to connect to /ws");

    // Give the server a moment to register the subscription before the
    // mutation fires, so the notification isn't sent before anyone's listening.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let wallet = Keypair::generate();
    let peer_id = peer_id_from_public_key(&wallet.public_key()).unwrap();
    let create = SessionCreate {
        id: openfiat_sessions::SessionId::new("sess-1"),
        wallet: peer_id.clone(),
        wallet_public_key: wallet.public_key(),
        client: "web".to_string(),
        host_node: peer_id,
        permissions: vec!["trade".to_string()],
        timestamp: Timestamp::now(),
        expires_at: Timestamp::from_millis(Timestamp::now().as_millis() + 3_600_000),
    };
    let signed = SignedSessionCreate::sign(create, &wallet);
    let data = openfiat_rpc::dispatch::encode_bytes(
        &openfiat_serialization::json::to_bytes(&signed).unwrap(),
    );

    handle
        .call("sendSessionEstablish", serde_json::json!({ "data": data }))
        .await
        .unwrap();

    let message = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("timed out waiting for a websocket message")
        .expect("stream ended without a message")
        .unwrap();

    let Message::Text(text) = message else {
        panic!("expected a text frame, got {message:?}")
    };
    let notification: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        notification["method"],
        serde_json::Value::from("sendSessionEstablish")
    );
    assert_eq!(notification["result"], serde_json::Value::from("sess-1"));

    ws.close(None).await.ok();
}
