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
use crate::onchain_stake;
use crate::state::NodeState;
use openfiat_chain::{ChainClient, NodeChainMode, RpcChainClient, SignatureStatus};
use openfiat_crypto::Keypair;
use openfiat_governance::events::SignedVoteCast;
use openfiat_network::{Multiaddr, Node};
use openfiat_notifications::{HttpGateway, NotificationProvider};
use openfiat_serialization::wire;
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

/// How often this node sweeps its own registry replica for stale services
/// (OFS-1500 §18). Purely local bookkeeping, so the cadence only bounds how
/// promptly a departed provider disappears — an hour against a multi-day
/// threshold is ample, and the sweep is a scan of a small column family.
const REGISTRY_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// `[PROPOSED — NEEDS SIGN-OFF]`: how long a service may go without a health
/// update before this node drops it.
///
/// OFS-1500 §18 pairs a 90-second expiry with §11's 30-second heartbeat, and
/// `protocol::EXPIRATION_THRESHOLD` carries that spec value. This is
/// deliberately ~6700x larger, because **nothing heartbeats yet**: until
/// `sendProviderHealthUpdate` was added alongside this constant there was no
/// way for a provider to refresh `last_health_update` at all, so every
/// registration still carries its registration timestamp. Sweeping at §18's
/// value today would evict the entire registry — measured against the live
/// devnet cluster, all 9 registered providers, none of which has ever sent a
/// health update.
///
/// Seven days leaves the existing population (oldest ~17h stale when this
/// shipped) a wide margin to adopt the new heartbeat path, while still
/// bounding the unbounded growth that motivated wiring the sweep on. Tighten
/// this toward §18's 90 seconds once providers heartbeat routinely; that is a
/// parameter change, not a code change.
const REGISTRY_EXPIRATION_THRESHOLD: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// How often this node drains the notifications its gossip handlers have
/// planned (`notify::NotificationDispatcher`) and hands them to the bound
/// gateways.
///
/// One second: notifications are user-facing, so latency is the whole
/// point, and an empty queue costs a timer wakeup. The queue is drained
/// in full each tick, so the interval bounds latency, not throughput.
const NOTIFICATION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// `[PROPOSED — NEEDS SIGN-OFF]`: how many `CHAIN_POLL_INTERVAL` ticks a
/// queued governance vote may go without a readable `StakeAccount` before
/// `poll_vote_verifications` gives up on it and says so.
///
/// 30 ticks is five minutes at the interval above. A vote's stake had to
/// exist on chain *before* the vote could honestly be cast, so five
/// minutes of "no such account" is a fabricated or closed account, not
/// replication lag — while still absorbing an RPC endpoint being down for
/// far longer than any single blockhash lives. The bound matters because
/// the queue is fed by gossip: without it, one peer emitting votes that
/// name accounts which will never exist grows this node's queue without
/// limit and without a word.
const VOTE_VERIFICATION_MAX_ATTEMPTS: u32 = 30;

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
                // See `methods::chain::SendTransactionParams`'s doc
                // comment for this tagging convention.
                match awaiting.correlation.as_deref().and_then(|tag| {
                    tag.split_once(':')
                        .map(|(domain, id)| (domain, id.to_string()))
                }) {
                    Some(("settlement", id)) => {
                        let settlement_id = openfiat_settlement::SettlementId::new(id);
                        let _ = state
                            .settlements
                            .apply_escrow_released(&settlement_id, awaiting.signature.clone());
                        // `EscrowReleased` is the one wired trigger with
                        // no gossip event behind it — the confirmation
                        // itself is the observation. Planned here, after
                        // the settlement has been updated, and delivered
                        // on the next `poll_notifications` tick.
                        state
                            .notification_dispatcher
                            .observe_escrow_release(&settlement_id, &awaiting.signature);
                    }
                    Some(("dispute", id)) => {
                        let _ = state.disputes.apply_onchain_execution(
                            &openfiat_disputes::DisputeId::new(id),
                            awaiting.signature.clone(),
                        );
                    }
                    _ => {}
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

/// One tick of the notification delivery path (OFS-6000): hand every
/// planned delivery to its bound gateway's endpoint, and record what the
/// handoff actually did.
///
/// The gossip handler that planned these deliveries could not perform
/// them itself — it runs synchronously inside the event loop, and an
/// unresponsive gateway would stall gossip and chain polling with it. So
/// planning and delivering are split, exactly the way `poll_chain` splits
/// submission from confirmation.
///
/// Every failure is contained here. `HttpGateway::send` has a hard
/// timeout, a failed handoff is recorded as `Failed` and dropped rather
/// than retried (the notification's own source event is long since
/// applied, and re-sending a stale one is worse than not), and nothing in
/// this function can affect any domain registry.
async fn poll_notifications<S: KvStore + 'static>(
    state: &NodeState<S>,
    provider: &dyn NotificationProvider,
) {
    for delivery in state.drain_notifications() {
        let outcome = provider.send(&delivery.endpoint, &delivery.payload).await;
        if let Err(error) = &outcome {
            eprintln!(
                "openfiat-notifications: handoff of {} to {} failed: {error}",
                delivery.payload.notification_id.as_str(),
                delivery.endpoint
            );
        }
        state
            .notifications
            .record_handoff(&delivery.payload.notification_id, outcome.is_ok());
    }
}

/// Independently verifies every queued governance vote's claimed stake
/// weight (`NodeState::drain_vote_verifications`) before it is ever
/// applied — the fix for `crates/governance`'s previously-documented
/// placeholder (trusting a client-signed `weight` outright, see
/// `VoteCast::weight`'s own doc). Runs against both this node's own
/// `sendVoteCast` submissions and every peer's gossiped `VoteCast`
/// (`NodeState::new`'s governance event-handler wiring queues both
/// identically), reading the voter's claimed `StakeAccount` PDA
/// directly rather than trusting anything the client asserts.
///
/// The owning program is [`openfiat_chain::PROGRAM_IDS`]`.staking`, a
/// compile-time constant — see that module for why an operator naming it
/// themselves would defeat this entire function.
///
/// Nothing here can end in silence. A claim is either applied, rejected
/// with a reason, or retried a bounded number of times and then dropped
/// with a reason; and `discard_unverifiable_votes` covers the one node
/// that can never run this at all.
async fn poll_vote_verifications<S: KvStore + 'static>(
    state: &NodeState<S>,
    client: &dyn ChainClient,
) {
    let staking_program_id = openfiat_chain::PROGRAM_IDS.staking;
    for mut pending in state.drain_vote_verifications() {
        match client.get_account(&pending.stake_account).await {
            Ok(Some((owner, data))) => {
                if owner != staking_program_id {
                    // The account exists but is not the staking program's
                    // — an attempt to pass off some other program's (or
                    // an attacker's own staking program's) account as
                    // protocol stake. Dropped, and said out loud: this is
                    // the exact attack the check exists for.
                    eprintln!(
                        "openfiat-rpc: rejected a governance vote — stake account {} is owned by \
                         {owner}, not the staking program {staking_program_id}",
                        pending.stake_account
                    );
                    continue;
                }
                match (
                    wire::from_bytes::<SignedVoteCast>(&pending.signed_vote_bytes),
                    onchain_stake::decode_stake_account(&data),
                ) {
                    (Ok(signed), Ok(decoded))
                        if decoded.owner == *signed.vote.voter_public_key.as_bytes() =>
                    {
                        let _ = state
                            .governance
                            .apply_vote_with_verified_weight(signed, decoded.amount);
                    }
                    _ => {
                        // Undecodable account layout, or a `StakeAccount`
                        // that belongs to somebody other than the voter
                        // who signed. Dropped for good, same as
                        // `poll_chain`'s handling of a confirmed-failed
                        // relay.
                        eprintln!(
                            "openfiat-rpc: rejected a governance vote — stake account {} does not \
                             decode as a StakeAccount belonging to the voter who signed",
                            pending.stake_account
                        );
                    }
                }
            }
            Ok(None) | Err(_) => {
                // Not yet observable (or a transient RPC error) — retry
                // on a later tick, same as `poll_chain`'s
                // `awaiting_confirmation` handling, but bounded: a stake
                // account that does not exist would otherwise be looked
                // up forever, and any peer can gossip a vote naming one.
                pending.attempts += 1;
                if pending.attempts >= VOTE_VERIFICATION_MAX_ATTEMPTS {
                    eprintln!(
                        "openfiat-rpc: gave up verifying a governance vote — stake account {} was \
                         still unreadable after {} attempts; the vote is not counted",
                        pending.stake_account, pending.attempts
                    );
                } else {
                    state.requeue_vote_verification(pending);
                }
            }
        }
    }
}

/// What a `GossipOnly` node does with the votes it can never verify.
///
/// Such a node has no RPC endpoint, so it cannot read a `StakeAccount` at
/// all — yet its gossip handler still queues every `VoteCast` it hears.
/// Holding them would grow that queue for the process's lifetime while
/// governance verification never once completed, and never said so: the
/// same silent stall the removed `staking_program_id: None` path had, just
/// arrived at by a different route. So they are dropped, loudly, with the
/// one thing that would change the outcome.
fn discard_unverifiable_votes<S: KvStore + 'static>(state: &NodeState<S>) {
    let discarded = state.drain_vote_verifications().len();
    if discarded > 0 {
        eprintln!(
            "openfiat-rpc: discarded {discarded} governance vote(s) — this node is GossipOnly and \
             cannot read on-chain stake, so it can never verify a vote's weight. Set \
             CLI_SOLANA_RPC_URLS to take part in governance tallying."
        );
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
            // Unconditional, unlike `chain_poll`: a GossipOnly node replicates
            // the registry just the same and must expire stale entries too.
            let mut registry_sweep = tokio::time::interval(REGISTRY_SWEEP_INTERVAL);
            // Unconditional, like `registry_sweep`: notifications are
            // driven by gossiped protocol events, which a `GossipOnly`
            // node sees just as well as an `RpcConnected` one.
            let mut notification_poll = tokio::time::interval(NOTIFICATION_POLL_INTERVAL);
            let notification_provider = HttpGateway::default();

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
                    // Deliberately unguarded, unlike this arm's earlier
                    // `if chain_client.is_some()`: a GossipOnly node has
                    // nothing to poll but still needs to be told, and to
                    // tell its operator, that the votes it is collecting
                    // can never be verified here.
                    _ = chain_poll.tick() => {
                        match chain_client.as_deref() {
                            Some(client) => {
                                poll_chain(&state, client).await;
                                poll_vote_verifications(&state, client).await;
                            }
                            None => discard_unverifiable_votes(&state),
                        }
                    }
                    _ = registry_sweep.tick() => {
                        state.services.expire_stale(REGISTRY_EXPIRATION_THRESHOLD);
                    }
                    _ = notification_poll.tick() => {
                        poll_notifications(&state, &notification_provider).await;
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
        async fn get_account(
            &self,
            _pubkey: &str,
        ) -> Result<Option<(String, Vec<u8>)>, ChainError> {
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
        async fn get_account(
            &self,
            _pubkey: &str,
        ) -> Result<Option<(String, Vec<u8>)>, ChainError> {
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
                Some(format!("settlement:{}", settlement_id.as_str())),
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

    /// A `ChainClient` fake reporting a fixed, well-formed `StakeAccount`
    /// for every `get_account` call — enough to prove
    /// `poll_vote_verifications` reads and trusts the *decoded* amount,
    /// not whatever a vote's own signed `weight` claims.
    ///
    /// `owner_program` is what the account claims to be owned by. There is
    /// no test-only way to tell `poll_vote_verifications` to accept some
    /// other program, and deliberately so — the fake has to produce the
    /// real pinned staking id to be believed, exactly as a real cluster
    /// would.
    struct StakeAccountChainClient {
        owner_program: String,
        owner: [u8; 32],
        amount: u64,
    }

    impl StakeAccountChainClient {
        /// An account genuinely owned by the pinned staking program.
        fn genuine(owner: [u8; 32], amount: u64) -> Self {
            Self {
                owner_program: openfiat_chain::PROGRAM_IDS.staking.to_string(),
                owner,
                amount,
            }
        }
    }

    #[async_trait::async_trait]
    impl ChainClient for StakeAccountChainClient {
        async fn get_latest_blockhash(&self) -> Result<(String, u64), ChainError> {
            Ok(("unused".to_string(), 0))
        }
        async fn is_blockhash_valid(&self, _blockhash: &str) -> Result<bool, ChainError> {
            Ok(true)
        }
        async fn send_transaction(&self, _tx_bytes: &[u8]) -> Result<String, ChainError> {
            Ok("unused".to_string())
        }
        async fn get_signature_status(
            &self,
            _signature: &str,
        ) -> Result<Option<SignatureStatus>, ChainError> {
            Ok(None)
        }
        async fn get_account(
            &self,
            _pubkey: &str,
        ) -> Result<Option<(String, Vec<u8>)>, ChainError> {
            Ok(Some((
                self.owner_program.clone(),
                crate::onchain_stake::fixture_stake_account_bytes(self.owner, self.amount),
            )))
        }
    }

    fn open_proposal(state: &NodeState<MemoryStore>) -> openfiat_governance::ProposalId {
        use openfiat_governance::events::{ProposalCreate, SignedProposalCreate};
        use openfiat_governance::{ProposalCategory, ProposalId};
        use openfiat_network::identity::peer_id_from_public_key;

        let author = Keypair::generate();
        let create = ProposalCreate {
            id: ProposalId::new("ofp-vote-verification-test"),
            title: "T".to_string(),
            summary: "S".to_string(),
            category: ProposalCategory::Protocol,
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            timestamp: openfiat_types::Timestamp::now(),
        };
        state
            .governance
            .apply_create(SignedProposalCreate::sign(create, &author))
            .unwrap()
    }

    #[tokio::test]
    async fn poll_vote_verifications_trusts_decoded_weight_not_the_votes_own_claim() {
        use openfiat_governance::VoteChoice;
        use openfiat_governance::events::{SignedVoteCast, VoteCast};

        let state = NodeState::new_for_test(MemoryStore::new());
        let proposal_id = open_proposal(&state);

        let voter = Keypair::generate();
        let vote = VoteCast {
            proposal_id: proposal_id.clone(),
            voter: openfiat_network::identity::peer_id_from_public_key(&voter.public_key())
                .unwrap(),
            voter_public_key: voter.public_key(),
            choice: VoteChoice::Approve,
            weight: 999_999, // an unverified, self-reported lie
            stake_account: "stake-1".to_string(),
            timestamp: openfiat_types::Timestamp::now(),
        };
        let signed = SignedVoteCast::sign(vote, &voter);
        let bytes = wire::to_bytes(&signed).unwrap();
        state.enqueue_vote_verification("stake-1".to_string(), bytes);

        let client = StakeAccountChainClient::genuine(*voter.public_key().as_bytes(), 42);
        poll_vote_verifications(&state, &client).await;

        let recorded_weight = state
            .governance
            .get(&proposal_id)
            .unwrap()
            .vote_by(
                &openfiat_network::identity::peer_id_from_public_key(&voter.public_key()).unwrap(),
            )
            .unwrap()
            .weight;
        assert_eq!(recorded_weight, 42);
    }

    #[tokio::test]
    async fn poll_vote_verifications_drops_a_claim_whose_owner_field_does_not_match_the_voter() {
        use openfiat_governance::VoteChoice;
        use openfiat_governance::events::{SignedVoteCast, VoteCast};

        let state = NodeState::new_for_test(MemoryStore::new());
        let proposal_id = open_proposal(&state);

        let voter = Keypair::generate();
        let someone_else = Keypair::generate();
        let vote = VoteCast {
            proposal_id: proposal_id.clone(),
            voter: openfiat_network::identity::peer_id_from_public_key(&voter.public_key())
                .unwrap(),
            voter_public_key: voter.public_key(),
            choice: VoteChoice::Approve,
            weight: 1,
            stake_account: "stake-1".to_string(),
            timestamp: openfiat_types::Timestamp::now(),
        };
        let signed = SignedVoteCast::sign(vote, &voter);
        let bytes = wire::to_bytes(&signed).unwrap();
        state.enqueue_vote_verification("stake-1".to_string(), bytes);

        // The claimed `StakeAccount` really belongs to `someone_else`,
        // not the voter who signed the vote.
        let client = StakeAccountChainClient::genuine(*someone_else.public_key().as_bytes(), 500);
        poll_vote_verifications(&state, &client).await;

        assert!(
            state
                .governance
                .get(&proposal_id)
                .unwrap()
                .vote_by(
                    &openfiat_network::identity::peer_id_from_public_key(&voter.public_key())
                        .unwrap()
                )
                .is_none(),
            "a stake account that doesn't belong to the voter must never be applied"
        );
    }

    /// Queues one signed vote for `proposal_id`, claiming `stake-1` as its
    /// stake account and an absurd self-reported weight — the shape every
    /// verification test starts from.
    fn queue_vote(
        state: &NodeState<MemoryStore>,
        voter: &Keypair,
        proposal_id: &openfiat_governance::ProposalId,
    ) {
        use openfiat_governance::VoteChoice;
        use openfiat_governance::events::{SignedVoteCast, VoteCast};

        let vote = VoteCast {
            proposal_id: proposal_id.clone(),
            voter: openfiat_network::identity::peer_id_from_public_key(&voter.public_key())
                .unwrap(),
            voter_public_key: voter.public_key(),
            choice: VoteChoice::Approve,
            weight: 10_000_000,
            stake_account: "stake-1".to_string(),
            timestamp: openfiat_types::Timestamp::now(),
        };
        let signed = SignedVoteCast::sign(vote, voter);
        state.enqueue_vote_verification("stake-1".to_string(), wire::to_bytes(&signed).unwrap());
    }

    /// The regression test for the reason this id is a constant: a node
    /// operator who deploys their own staking program and mints themselves
    /// a `StakeAccount` with any balance they like must not be able to
    /// make their node count it. Before, the owning program came from
    /// `CLI_STAKING_PROGRAM_ID` and this account would have been believed.
    #[tokio::test]
    async fn a_stake_account_owned_by_some_other_staking_program_is_never_counted() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let proposal_id = open_proposal(&state);
        let voter = Keypair::generate();
        queue_vote(&state, &voter, &proposal_id);

        // Perfectly well-formed, genuinely the voter's own account, with a
        // large balance — and owned by a program that is not ours.
        let client = StakeAccountChainClient {
            owner_program: "TESTPRoGRAM1111111111111111111111111111111".to_string(),
            owner: *voter.public_key().as_bytes(),
            amount: 1_000_000_000,
        };
        poll_vote_verifications(&state, &client).await;

        assert!(
            state
                .governance
                .get(&proposal_id)
                .unwrap()
                .vote_by(
                    &openfiat_network::identity::peer_id_from_public_key(&voter.public_key())
                        .unwrap()
                )
                .is_none(),
            "only the pinned staking program's accounts may carry vote weight"
        );
        assert!(
            state.drain_vote_verifications().is_empty(),
            "a rejected claim is dropped, not retried"
        );
    }

    /// `FakeChainClient::get_account` answers `Ok(None)` forever, standing
    /// in for a stake account that does not exist — a claim any peer can
    /// gossip. It must be retried, but not indefinitely.
    #[tokio::test]
    async fn an_unreadable_stake_account_is_retried_a_bounded_number_of_times_then_dropped() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let proposal_id = open_proposal(&state);
        queue_vote(&state, &Keypair::generate(), &proposal_id);
        let client = FakeChainClient {
            blockhash: "unused",
            slot: 0,
        };

        for _ in 0..VOTE_VERIFICATION_MAX_ATTEMPTS - 1 {
            poll_vote_verifications(&state, &client).await;
        }
        let still_queued = state.drain_vote_verifications();
        assert_eq!(still_queued.len(), 1, "must keep retrying below the bound");
        assert_eq!(
            still_queued[0].attempts,
            VOTE_VERIFICATION_MAX_ATTEMPTS - 1,
            "a retry must carry its attempt count forward, not reset it"
        );
        state.requeue_vote_verification(still_queued.into_iter().next().unwrap());

        poll_vote_verifications(&state, &client).await;
        assert!(
            state.drain_vote_verifications().is_empty(),
            "at the bound the claim is given up on, not queued forever"
        );
    }

    /// A `GossipOnly` node collects votes it can never verify. It must not
    /// accumulate them silently for the lifetime of the process.
    #[tokio::test]
    async fn a_gossip_only_node_discards_the_votes_it_can_never_verify() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let proposal_id = open_proposal(&state);
        queue_vote(&state, &Keypair::generate(), &proposal_id);

        discard_unverifiable_votes(&state);

        assert!(
            state.drain_vote_verifications().is_empty(),
            "an unverifiable vote must be dropped, not held forever"
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

    /// A `NotificationProvider` that records what it was asked to send
    /// and answers with a configured outcome — so `poll_notifications`'s
    /// own drain-and-record logic is testable without a socket. The real
    /// HTTP behaviour (timeouts, non-2xx, the exact bytes on the wire) is
    /// covered against a real server in `openfiat_notifications::gateway`.
    struct RecordingProvider {
        sent: std::sync::Mutex<Vec<(String, openfiat_notifications::NotificationId)>>,
        outcome: Result<(), openfiat_notifications::NotificationError>,
    }

    #[async_trait::async_trait]
    impl openfiat_notifications::NotificationProvider for RecordingProvider {
        fn channels(&self) -> Vec<openfiat_types::NotificationChannel> {
            vec![openfiat_types::NotificationChannel::Email]
        }
        async fn send(
            &self,
            endpoint: &str,
            payload: &openfiat_notifications::NotificationPayload,
        ) -> Result<(), openfiat_notifications::NotificationError> {
            self.sent
                .lock()
                .unwrap()
                .push((endpoint.to_string(), payload.notification_id.clone()));
            self.outcome.clone()
        }
    }

    /// Builds a node with one gateway registered, one subscribed wallet,
    /// and one planned delivery already queued.
    fn state_with_one_queued_notification() -> (
        NodeState<MemoryStore>,
        openfiat_notifications::NotificationId,
    ) {
        use openfiat_crypto::seal;
        use openfiat_network::identity::peer_id_from_public_key;
        use openfiat_notifications::events::{SignedSubscriptionUpdate, SubscriptionUpdate};
        use openfiat_notifications::{NotificationCategory, SubscriptionDestination};
        use openfiat_registry::{Registration, SignedRegistration};
        use openfiat_types::{NotificationChannel, ServiceId, ServiceType, Timestamp};

        let state = NodeState::new_for_test(MemoryStore::new());
        let gateway = Keypair::generate();
        let wallet = Keypair::generate();
        let wallet_id = peer_id_from_public_key(&wallet.public_key()).unwrap();

        state
            .services
            .apply_registration(SignedRegistration::sign(
                Registration {
                    service_id: ServiceId::new("gw-1"),
                    service_type: ServiceType::Notifications(NotificationChannel::Email),
                    provider: peer_id_from_public_key(&gateway.public_key()).unwrap(),
                    provider_public_key: gateway.public_key(),
                    endpoints: vec!["https://gw.example/deliver".to_string()],
                    supported_ofs: vec![6000],
                    region: None,
                    capabilities: vec![],
                    pricing: None,
                    payout_wallet: None,
                    timestamp: Timestamp::now(),
                },
                &gateway,
            ))
            .unwrap();
        state
            .notifications
            .apply_subscription_update(SignedSubscriptionUpdate::sign(
                SubscriptionUpdate {
                    wallet: wallet_id.clone(),
                    wallet_public_key: wallet.public_key(),
                    enabled_categories: vec![NotificationCategory::Trading],
                    destinations: vec![SubscriptionDestination {
                        service_id: ServiceId::new("gw-1"),
                        channel: NotificationChannel::Email,
                        sealed: seal(&gateway.public_key(), b"user@example.com").unwrap(),
                    }],
                    timestamp: Timestamp::now(),
                },
                &wallet,
            ))
            .unwrap();

        let plan = state.notifications.plan(
            openfiat_notifications::NotificationTrigger::SettlementApproved,
            b"source-event",
            &wallet_id,
        );
        let delivery = plan.deliveries.into_iter().next().unwrap();
        let id = delivery.payload.notification_id.clone();
        state.notifications.record_queued(&delivery);
        state.enqueue_notification(delivery);
        (state, id)
    }

    #[tokio::test]
    async fn poll_notifications_hands_every_queued_delivery_to_its_gateway() {
        let (state, id) = state_with_one_queued_notification();
        let provider = RecordingProvider {
            sent: std::sync::Mutex::new(Vec::new()),
            outcome: Ok(()),
        };

        poll_notifications(&state, &provider).await;

        assert_eq!(
            provider.sent.lock().unwrap().as_slice(),
            &[("https://gw.example/deliver".to_string(), id.clone())]
        );
        assert_eq!(
            state.notifications.dispatch(&id).unwrap().status,
            openfiat_notifications::DeliveryStatus::Sent
        );
        assert!(
            state.drain_notifications().is_empty(),
            "a handed-off delivery must not be re-sent on the next tick"
        );
    }

    #[tokio::test]
    async fn a_failed_handoff_is_recorded_and_not_retried() {
        let (state, id) = state_with_one_queued_notification();
        let provider = RecordingProvider {
            sent: std::sync::Mutex::new(Vec::new()),
            outcome: Err(openfiat_notifications::NotificationError::ProviderUnavailable),
        };

        poll_notifications(&state, &provider).await;

        assert_eq!(
            state.notifications.dispatch(&id).unwrap().status,
            openfiat_notifications::DeliveryStatus::Failed
        );
        assert!(state.drain_notifications().is_empty());

        // The second tick must be a no-op, not a re-send: the source
        // event is long applied, and replaying a stale notification is
        // worse than dropping it.
        poll_notifications(&state, &provider).await;
        assert_eq!(provider.sent.lock().unwrap().len(), 1);
    }
}
