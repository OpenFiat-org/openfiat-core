//! Bridges this crate's `Rc`-based [`NodeState`] (and every domain
//! registry it composes) into axum's multi-threaded world.
//!
//! Every other crate in this workspace deliberately uses `Rc`, not
//! `Arc` — the whole registry/service layer is single-threaded by
//! design, matching how one P2P node's event loop naturally runs. That
//! makes it unsound to share `NodeState` directly via axum's `State`
//! extractor (axum/hyper's connection handling uses `tokio::spawn`,
//! which requires `Send` futures even under a `current_thread` runtime).
//!
//! Instead, `NodeState` and the method table live entirely inside one
//! dedicated OS thread running its own single-threaded Tokio runtime.
//! Axum handlers hold an [`RpcHandle`] — a plain `mpsc::UnboundedSender`,
//! `Clone + Send + Sync` — and talk to that thread over a channel, the
//! same actor pattern used to bridge any `!Send` resource into an async
//! multi-threaded server.

use crate::dispatch::MethodTable;
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_storage::KvStore;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};

struct RpcCommand {
    method: String,
    params: Value,
    respond_to: oneshot::Sender<Result<Value, RpcError>>,
}

/// A `Send + Sync` handle to the actor thread — this, not `NodeState`, is
/// what axum's `State` extractor holds.
#[derive(Clone)]
pub struct RpcHandle {
    sender: mpsc::UnboundedSender<RpcCommand>,
    events: broadcast::Sender<Value>,
}

impl RpcHandle {
    pub async fn call(&self, method: impl Into<String>, params: Value) -> Result<Value, RpcError> {
        let (respond_to, response) = oneshot::channel();
        self.sender
            .send(RpcCommand {
                method: method.into(),
                params,
                respond_to,
            })
            .map_err(|_| RpcError::Internal("RPC actor is no longer running".to_string()))?;
        response
            .await
            .map_err(|_| RpcError::Internal("RPC actor dropped the response".to_string()))?
    }

    /// A generic firehose of every successful `sendX` mutation's result,
    /// as `{"method": "...", "result": ...}` — the one working WebSocket
    /// subscription this crate's exit criterion asks for. Per-topic
    /// filtering (a specific reservation, a specific oracle pair) is a
    /// natural extension a client can do today by filtering client-side;
    /// nothing here rules it out later.
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }
}

/// Spawns the actor thread. `build_store` runs *inside* that thread, not
/// before — `S` (and everything built from it) never needs to be `Send`,
/// only the construction closure does.
pub fn spawn_actor<S>(build_store: impl FnOnce() -> S + Send + 'static) -> RpcHandle
where
    S: KvStore + 'static,
{
    let (sender, mut receiver) = mpsc::unbounded_channel::<RpcCommand>();
    let (events, _) = broadcast::channel::<Value>(1024);
    let handle = RpcHandle {
        sender,
        events: events.clone(),
    };

    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to start the RPC actor's runtime");
        runtime.block_on(async move {
            let state = NodeState::new(build_store());
            let table: MethodTable<S> = crate::methods::build_table();

            while let Some(command) = receiver.recv().await {
                let result = table.dispatch(&state, &command.method, command.params);
                if command.method.starts_with("send")
                    && let Ok(value) = &result
                {
                    let _ = events
                        .send(serde_json::json!({ "method": command.method, "result": value }));
                }
                let _ = command.respond_to.send(result);
            }
        });
    });

    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_storage::mem::MemoryStore;

    #[tokio::test]
    async fn a_call_reaches_the_actor_and_returns_a_result() {
        let handle = spawn_actor(MemoryStore::new);
        let result = handle.call("getVersion", Value::Null).await.unwrap();
        assert!(result["version"].is_string());
    }

    #[tokio::test]
    async fn an_unknown_method_returns_method_not_found() {
        let handle = spawn_actor(MemoryStore::new);
        let result = handle.call("doesNotExist", Value::Null).await;
        assert!(matches!(result, Err(RpcError::MethodNotFound(_))));
    }

    #[tokio::test]
    async fn a_subscriber_receives_a_notification_for_a_successful_send_method() {
        let handle = spawn_actor(MemoryStore::new);
        let mut subscription = handle.subscribe();

        // getVersion doesn't start with "send", so it must not notify.
        handle.call("getVersion", Value::Null).await.unwrap();
        assert!(subscription.try_recv().is_err());

        use crate::dispatch::encode_bytes;
        use openfiat_crypto::Keypair;
        use openfiat_network::identity::peer_id_from_public_key;
        use openfiat_sessions::events::{SessionCreate, SignedSessionCreate};
        use openfiat_types::Timestamp;

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
        let data = encode_bytes(&openfiat_serialization::wire::to_bytes(&signed).unwrap());

        let result = handle
            .call("sendSessionEstablish", serde_json::json!({ "data": data }))
            .await
            .unwrap();
        assert_eq!(result, Value::from("sess-1"));

        let notification = subscription
            .try_recv()
            .expect("expected a notification for a successful sendX call");
        assert_eq!(notification["method"], Value::from("sendSessionEstablish"));
        assert_eq!(notification["result"], Value::from("sess-1"));
    }
}
