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
use openfiat_chain::{ChainClient, NodeChainMode, RpcChainClient, SignatureStatus};
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
/// §6-7): fetch and announce a fresh blockhash, submit whatever is
/// queued in `state.chain`'s pending-relay queue — a caller's own
/// `sendTransaction`, or a `GossipOnly` peer's relayed request (see
/// `NodeState::new`'s wiring of both into the same queue) — then poll
/// every submitted-but-unconfirmed signature for real on-chain finality.
///
/// Submission acceptance and confirmation are deliberately two separate
/// steps: `ChainClient::send_transaction` succeeding only means the RPC
/// endpoint accepted it for processing, not that it has landed. Treating
/// that as "confirmed" (this crate's own earlier behavior) would let a
/// caller's `settlement_id` correlation fire `apply_escrow_released`
/// before the funds have actually moved. Every
/// `state.gossip.borrow_mut()` here is a short-lived temporary scoped to
/// one synchronous statement, never spanning an `.await`, so this needs
/// no `RefCell`-across-await allowance the way `drive_gossip` does.
async fn poll_chain<S: KvStore + 'static>(state: &NodeState<S>, client: &dyn ChainClient) {
    if let Ok((blockhash, slot)) = client.get_latest_blockhash().await {
        let _ =
            state
                .chain_bridge
                .announce_blockhash(&mut state.gossip.borrow_mut(), &blockhash, slot);
    }

    for pending in state.chain.drain_pending_relay() {
        if let Ok(signature) = client.send_transaction(&pending.tx_bytes).await {
            let slot_submitted = state.chain.current_blockhash().map_or(0, |(_, slot)| slot);
            state
                .chain
                .track_awaiting_confirmation(signature, slot_submitted, pending.correlation);
        }
        // A failed submission is silently dropped (OFS-4300's own relay
        // path is explicitly best-effort) — the bytes aren't re-queued
        // since a signed transaction's blockhash eventually expires
        // anyway, and the caller (or a `GossipOnly` peer awaiting relay)
        // can resubmit against a fresher one.
    }

    for awaiting in state.chain.awaiting_confirmations() {
        let status = client.get_signature_status(&awaiting.signature).await;
        match status {
            Ok(Some(SignatureStatus::Success)) => {
                state.chain.resolve_confirmation(&awaiting.signature);
                let _ = state.chain_bridge.announce_relay_confirmation(
                    &mut state.gossip.borrow_mut(),
                    &awaiting.signature,
                    awaiting.slot_submitted,
                );
                if let Some(settlement_id) = &awaiting.correlation {
                    let _ = state.settlements.apply_escrow_released(
                        &openfiat_settlement::SettlementId::new(settlement_id.clone()),
                        awaiting.signature.clone(),
                    );
                }
            }
            Ok(Some(SignatureStatus::Failed)) => {
                // A confirmed failure — stop polling it, same "no
                // re-queue" reasoning as a failed submission above.
                state.chain.resolve_confirmation(&awaiting.signature);
            }
            Ok(None) | Err(_) => {
                // Not yet observed (or a transient RPC error) — stays in
                // `awaiting_confirmation` for the next tick.
            }
        }
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
            let node =
                Node::new(&network.keypair).expect("failed to start this node's libp2p transport");
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
    use openfiat_chain::ChainError;
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

    /// A `ChainClient` fake whose `get_signature_status` reports "not yet
    /// seen" on its first call and "confirmed" from then on — so a test
    /// can prove `poll_chain` genuinely waits for real confirmation
    /// rather than treating mere submission-acceptance as final.
    struct SlowConfirmChainClient {
        blockhash: &'static str,
        slot: u64,
        status_calls: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl ChainClient for SlowConfirmChainClient {
        async fn get_latest_blockhash(&self) -> Result<(String, u64), ChainError> {
            Ok((self.blockhash.to_string(), self.slot))
        }
        async fn is_blockhash_valid(&self, _blockhash: &str) -> Result<bool, ChainError> {
            Ok(true)
        }
        async fn send_transaction(&self, _tx_bytes: &[u8]) -> Result<String, ChainError> {
            Ok("real-looking-signature".to_string())
        }
        async fn get_signature_status(
            &self,
            _signature: &str,
        ) -> Result<Option<SignatureStatus>, ChainError> {
            let calls = self
                .status_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if calls == 0 {
                Ok(None) // not yet observed on this first check
            } else {
                Ok(Some(SignatureStatus::Success))
            }
        }
        async fn get_account(&self, _pubkey: &str) -> Result<Option<Vec<u8>>, ChainError> {
            Ok(None)
        }
    }

    /// A real (if minimal) bincode-serialized `VersionedTransaction` —
    /// `ChainState::enqueue_relay` validates against this exact shape
    /// (OFS-4300 §7), so an arbitrary byte string doesn't pass.
    fn fixture_transaction_bytes() -> Vec<u8> {
        use solana_signer::Signer as _;
        let payer = solana_keypair::Keypair::new();
        let message = solana_message::Message::new(&[], Some(&payer.pubkey()));
        let mut tx = solana_transaction::Transaction::new_unsigned(message);
        tx.sign(&[&payer], solana_hash::Hash::default());
        let versioned = solana_transaction::versioned::VersionedTransaction::from(tx);
        bincode::serialize(&versioned)
            .expect("a freshly-built VersionedTransaction always serializes")
    }

    /// Builds a `Settlement` in the `Approved` state the same way
    /// `crates/conformance`'s `trade_lifecycle` test does (Initiate ->
    /// PaymentSubmitted -> Approved), directly against the registry
    /// rather than through gossip — `poll_chain`'s own logic is what's
    /// under test here, not replication.
    fn approved_settlement(
        state: &NodeState<openfiat_storage::mem::MemoryStore>,
    ) -> openfiat_settlement::SettlementId {
        use openfiat_crypto::Keypair;
        use openfiat_network::identity::peer_id_from_public_key;
        use openfiat_settlement::SettlementId;
        use openfiat_settlement::events::{
            PaymentSubmitted, SettlementApproved, SettlementInitiate, SignedPaymentSubmitted,
            SignedSettlementApproved, SignedSettlementInitiate,
        };
        use openfiat_types::{Amount, Timestamp};

        let buyer = Keypair::from_seed([3u8; 32]);
        let seller = Keypair::from_seed([4u8; 32]);
        let buyer_peer = peer_id_from_public_key(&buyer.public_key()).unwrap();
        let seller_peer = peer_id_from_public_key(&seller.public_key()).unwrap();
        let settlement_id = SettlementId::new("set-poll-chain-test");

        let initiate = SettlementInitiate {
            id: settlement_id.clone(),
            reservation_id: openfiat_reservations::ReservationId::new("res-poll-chain-test"),
            buyer: buyer_peer.clone(),
            buyer_public_key: buyer.public_key(),
            seller: seller_peer.clone(),
            seller_public_key: seller.public_key(),
            amount: Amount::new(50_00, 2),
            timestamp: Timestamp::now(),
        };
        state
            .settlements
            .apply_initiate(SignedSettlementInitiate::sign(initiate, &buyer))
            .unwrap();

        let payment = PaymentSubmitted {
            settlement_id: settlement_id.clone(),
            buyer: buyer_peer,
            payment_reference: Some("REF-1".to_string()),
            timestamp: Timestamp::now(),
        };
        state
            .settlements
            .apply_payment_submitted(SignedPaymentSubmitted::sign(payment, &buyer))
            .unwrap();

        let approved = SettlementApproved {
            settlement_id: settlement_id.clone(),
            seller: seller_peer,
            timestamp: Timestamp::now(),
        };
        state
            .settlements
            .apply_approved(SignedSettlementApproved::sign(approved, &seller))
            .unwrap();

        settlement_id
    }

    #[tokio::test]
    async fn poll_chain_only_records_escrow_release_once_real_confirmation_is_observed() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let settlement_id = approved_settlement(&state);

        state
            .chain
            .enqueue_relay(
                fixture_transaction_bytes(),
                Some(settlement_id.as_str().to_string()),
            )
            .unwrap();

        let client = SlowConfirmChainClient {
            blockhash: "fake-blockhash-456",
            slot: 999,
            status_calls: std::sync::atomic::AtomicU32::new(0),
        };

        // Tick 1: submits the transaction (now awaiting confirmation) and
        // checks status once — `SlowConfirmChainClient` reports "not yet
        // seen", so nothing should be recorded yet.
        poll_chain(&state, &client).await;
        assert_eq!(
            state
                .settlements
                .get(&settlement_id)
                .unwrap()
                .escrow_release_signature,
            None,
            "must not record a release before real confirmation is observed"
        );
        assert_eq!(state.chain.awaiting_confirmations().len(), 1);

        // Tick 2: the same signature is still awaiting, and this time the
        // fake client reports it confirmed.
        poll_chain(&state, &client).await;
        let settlement = state.settlements.get(&settlement_id).unwrap();
        assert_eq!(
            settlement.escrow_release_signature,
            Some("real-looking-signature".to_string())
        );
        assert!(state.chain.awaiting_confirmations().is_empty());
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
