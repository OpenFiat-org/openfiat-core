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
use openfiat_chain::{ChainClient, NodeChainMode, RpcChainClient};
use openfiat_crypto::Keypair;
use openfiat_network::{Multiaddr, Node};
use openfiat_storage::KvStore;
use openfiat_types::NodeRole;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};

/// `[PROPOSED — NEEDS SIGN-OFF]`: how often an `RpcConnected` node polls
/// its configured Solana RPC endpoint(s) for a fresh blockhash and drains
/// any pending transaction relay. Solana's own blockhash validity window
/// is ~150 slots (~60-90s at ~400ms/slot — OFS-4300 §6), so this is
/// conservative headroom rather than a tight deadline, and stays gentle
/// on a shared/rate-limited public RPC endpoint.
const CHAIN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

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

/// This node's real network identity and cluster-bootstrap configuration
/// — everything `spawn_actor` needs to bring up a genuine, gossip-connected
/// [`NodeState`] instead of an isolated local registry. Plain data (no
/// `Rc`/`RefCell`), so it can cross into the actor thread the same way
/// `build_store`'s closure does.
pub struct NetworkConfig {
    pub keypair: Keypair,
    pub self_roles: Vec<NodeRole>,
    pub listen_addr: Multiaddr,
    pub bootstrap_peers: Vec<Multiaddr>,
    pub chain_mode: NodeChainMode,
}

impl NetworkConfig {
    /// A lone, unbootstrapped node on an OS-assigned loopback port — for
    /// tests that only need a real transport to exist, not a cluster.
    /// Not `cfg(test)`-gated: this crate's own integration tests (under
    /// `tests/`) link against a normal build, not a test-cfg'd one.
    pub fn for_test() -> Self {
        Self {
            keypair: Keypair::generate(),
            self_roles: Vec::new(),
            listen_addr: "/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap(),
            bootstrap_peers: Vec::new(),
            chain_mode: NodeChainMode::GossipOnly,
        }
    }
}

/// Drives one iteration of this node's gossip network loop. Holds
/// `state.gossip`'s `RefCell` guard across the internal `.await` — sound
/// only because the sole caller is a `tokio::select!` arm (see
/// `spawn_actor`): losing the race cancels (drops) this future outright
/// before `dispatch`'s own arm ever runs, so the two paths never actually
/// interleave a concurrent borrow the way the lint otherwise guards
/// against.
#[allow(clippy::await_holding_refcell_ref)]
async fn drive_gossip<S: KvStore + 'static>(state: &NodeState<S>) {
    state.gossip.borrow_mut().drive_once().await;
}

/// One tick of an `RpcConnected` node's Solana connectivity (OFS-4300
/// §6-7): fetch and announce a fresh blockhash, then submit whatever is
/// queued in `state.chain`'s pending-relay queue — a caller's own
/// `sendTransaction`, or a `GossipOnly` peer's relayed request (see
/// `NodeState::new`'s wiring of both into the same queue). Every
/// `state.gossip.borrow_mut()` here is a short-lived temporary scoped to
/// one synchronous statement, never spanning an `.await`, so this needs
/// no `RefCell`-across-await allowance the way `drive_gossip` does.
async fn poll_chain<S: KvStore + 'static>(state: &NodeState<S>, client: &dyn ChainClient) {
    if let Ok((blockhash, slot)) = client.get_latest_blockhash().await {
        let _ = state
            .chain_bridge
            .announce_blockhash(&mut state.gossip.borrow_mut(), &blockhash, slot);
    }

    for tx_bytes in state.chain.drain_pending_relay() {
        if let Ok(signature) = client.send_transaction(&tx_bytes).await {
            let slot_submitted = state.chain.current_blockhash().map_or(0, |(_, slot)| slot);
            let _ = state.chain_bridge.announce_relay_confirmation(
                &mut state.gossip.borrow_mut(),
                &signature,
                slot_submitted,
            );
        }
        // A failed submission is silently dropped (OFS-4300's own relay
        // path is explicitly best-effort) — the bytes aren't re-queued
        // since a signed transaction's blockhash eventually expires
        // anyway, and the caller (or a `GossipOnly` peer awaiting relay)
        // can resubmit against a fresher one.
    }
}

/// Spawns the actor thread. `build_store` and `network` both take effect
/// *inside* that thread, not before — `S`, `Node`, and everything built
/// from them never needs to be `Send`, only the values handed in do.
pub fn spawn_actor<S>(
    build_store: impl FnOnce() -> S + Send + 'static,
    network: NetworkConfig,
) -> RpcHandle
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
            let node = Node::new(&network.keypair)
                .expect("failed to start this node's libp2p transport");
            let chain_client: Option<Box<dyn ChainClient>> = match &network.chain_mode {
                NodeChainMode::RpcConnected { rpc_urls, .. } => {
                    Some(Box::new(RpcChainClient::new(rpc_urls.clone())))
                }
                NodeChainMode::GossipOnly => None,
            };
            let state = NodeState::new(
                node,
                build_store(),
                network.keypair,
                network.self_roles,
                network.chain_mode,
            );
            {
                let mut gossip = state.gossip.borrow_mut();
                gossip
                    .node
                    .listen_on(network.listen_addr)
                    .expect("failed to bind this node's gossip listen address");
                for peer in network.bootstrap_peers {
                    gossip
                        .node
                        .dial(peer)
                        .expect("failed to dial a configured bootstrap peer");
                }
            }
            let table: MethodTable<S> = crate::methods::build_table();
            let mut chain_poll = tokio::time::interval(CHAIN_POLL_INTERVAL);

            loop {
                tokio::select! {
                    command = receiver.recv() => {
                        let Some(command) = command else { break };
                        let result = table.dispatch(&state, &command.method, command.params);
                        if command.method.starts_with("send")
                            && let Ok(value) = &result
                        {
                            let _ = events.send(
                                serde_json::json!({ "method": command.method, "result": value }),
                            );
                        }
                        let _ = command.respond_to.send(result);
                    }
                    _ = drive_gossip(&state) => {}
                    _ = chain_poll.tick(), if chain_client.is_some() => {
                        poll_chain(&state, chain_client.as_deref().unwrap()).await;
                    }
                }
            }
        });
    });

    handle
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_chain::{ChainError, SignatureStatus};
    use openfiat_storage::mem::MemoryStore;

    /// A `ChainClient` fake, so `poll_chain`'s own glue logic (fetch,
    /// announce, drain, submit) is testable without a live Solana
    /// cluster — the real `RpcChainClient` is exercised end to end
    /// against actual devnet by this phase's own manual verification and
    /// `crates/conformance`'s Phase VII suite.
    struct FakeChainClient {
        blockhash: &'static str,
        slot: u64,
    }

    #[async_trait::async_trait]
    impl ChainClient for FakeChainClient {
        async fn get_latest_blockhash(&self) -> Result<(String, u64), ChainError> {
            Ok((self.blockhash.to_string(), self.slot))
        }
        async fn is_blockhash_valid(&self, _blockhash: &str) -> Result<bool, ChainError> {
            Ok(true)
        }
        async fn send_transaction(&self, _tx_bytes: &[u8]) -> Result<String, ChainError> {
            Ok("fake-signature".to_string())
        }
        async fn get_signature_status(
            &self,
            _signature: &str,
        ) -> Result<Option<SignatureStatus>, ChainError> {
            Ok(Some(SignatureStatus::Success))
        }
        async fn get_account(&self, _pubkey: &str) -> Result<Option<Vec<u8>>, ChainError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn poll_chain_announces_a_fresh_blockhash_the_rpc_layer_then_reports() {
        let state = NodeState::new_for_test(MemoryStore::new());
        assert_eq!(state.chain.current_blockhash(), None);

        let client = FakeChainClient {
            blockhash: "fake-blockhash-123",
            slot: 555,
        };
        poll_chain(&state, &client).await;

        assert_eq!(
            state.chain.current_blockhash(),
            Some(("fake-blockhash-123".to_string(), 555)),
            "poll_chain's announce_blockhash call must reach ChainState via the same \
             BlockhashAnnounced event handler a peer's announcement would"
        );
    }

    #[tokio::test]
    async fn a_call_reaches_the_actor_and_returns_a_result() {
        let handle = spawn_actor(MemoryStore::new, NetworkConfig::for_test());
        let result = handle.call("getVersion", Value::Null).await.unwrap();
        assert!(result["version"].is_string());
    }

    #[tokio::test]
    async fn an_unknown_method_returns_method_not_found() {
        let handle = spawn_actor(MemoryStore::new, NetworkConfig::for_test());
        let result = handle.call("doesNotExist", Value::Null).await;
        assert!(matches!(result, Err(RpcError::MethodNotFound(_))));
    }

    #[tokio::test]
    async fn a_subscriber_receives_a_notification_for_a_successful_send_method() {
        let handle = spawn_actor(MemoryStore::new, NetworkConfig::for_test());
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
        let data = encode_bytes(&openfiat_serialization::json::to_bytes(&signed).unwrap());

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
