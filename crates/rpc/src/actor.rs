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
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use openfiat_chain::{ChainClient, NodeChainMode, RpcChainClient, SignatureStatus};
use openfiat_crypto::Keypair;
use openfiat_governance::events::SignedVoteCast;
use openfiat_network::{Multiaddr, Node};
use openfiat_notifications::{HttpGateway, NotificationProvider};
use openfiat_serialization::wire;
use openfiat_snapshot::SnapshotConfig;
use openfiat_storage::KvStore;
use openfiat_types::NodeRole;
use serde_json::Value;
use std::rc::Rc;
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
    /// Where this node keeps snapshots, how often it writes one, and the
    /// public URL it announces them under (OFS-1300). Defaults to
    /// producing nothing — see [`SnapshotConfig::produces`].
    pub snapshot: SnapshotConfig,
    /// An IPFS daemon this node pins protocol content through, from
    /// `--ipfs-api-url`. `None` means the operator did not opt in: the
    /// node stores no content, answers no challenge, and earns the
    /// reduced `pinning_absent_bps` share. Opting in is what earns the
    /// premium, and it is a real cost to the operator, so it is never
    /// assumed.
    pub ipfs_api_url: Option<String>,
    /// How long this node keeps the content it pins. Defaults to a
    /// bounded rolling window — running a node should not be an
    /// open-ended storage commitment.
    pub retention: openfiat_content::Retention,
    /// This node's publicly reachable API URL, if it has one.
    ///
    /// `Some` means the operator has put the node behind TLS and wants it
    /// used: the node advertises itself in the service registry
    /// (OFS-1500) as a `PublicApiNode`, and browsers and other clients
    /// discover it there. `None` is the ordinary case for a node on a
    /// laptop or behind a firewall, and advertises nothing.
    pub public_rpc_url: Option<String>,
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
            ipfs_api_url: None,
            retention: openfiat_content::Retention::default(),
            public_rpc_url: None,
            snapshot: SnapshotConfig::default(),
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
    let mut gossip = state.gossip.borrow_mut();
    gossip.drive_once().await;

    // Report where this node turned out to be reachable, once per address.
    //
    // It cannot be known at startup. `--gossip-bind-address` defaults to
    // the wildcard `0.0.0.0`, which is a listening instruction rather than
    // an address — nothing can dial it — so the real answer only exists
    // after libp2p has expanded it across the host's interfaces, and the
    // public one only after a peer has told us what it saw. Printing the
    // bind address as though it were an entrypoint, which this node used
    // to do, hands an operator a string that fails on the far side with no
    // hint as to why.
    // A cloned identity is the loudest thing this node can discover about
    // itself, and it is discovered here or nowhere: only the node holding
    // the key can tell an event it did not emit from one it did.
    let conflicts = gossip.identity_conflicts();
    if conflicts > 0 && conflicts != state.reported_identity_conflicts.get() {
        state.reported_identity_conflicts.set(conflicts);
        tracing::error!(
            events = conflicts,
            "ANOTHER NODE IS RUNNING THIS WALLET. Events signed by this \
             node's own key, which this node did not emit, are arriving \
             from the network. One wallet is one node: a second node on \
             the same identity splits this node's stake and reputation \
             across two machines and makes a compromise of either \
             indistinguishable. Stop the other node, or give it its own \
             identity with `solana-keygen new`. Those events are being \
             rejected, not applied."
        );
    }

    let peer_id = gossip.node.libp2p_peer_id();
    for address in gossip.take_newly_reachable() {
        tracing::info!(
            entrypoint = %format!("{address}/p2p/{peer_id}"),
            "reachable at a new address; peers can use this as --entrypoint"
        );
    }
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
    match client.get_latest_blockhash().await {
        Ok((blockhash, slot)) => {
            tracing::debug!(slot, %blockhash, "announcing blockhash to peers");
            let _ = state.chain_bridge.announce_blockhash(
                &mut state.gossip.borrow_mut(),
                &blockhash,
                slot,
            );
        }
        // WARN, not DEBUG: an RpcConnected node that cannot read the chain
        // is the one every GossipOnly peer is relying on for on-chain
        // truth. It keeps serving cached state, so nothing else about the
        // node looks wrong.
        Err(err) => tracing::warn!(
            ?err,
            "could not read the chain — peers relying on this node for on-chain facts will go stale"
        ),
    }

    for pending in state.chain.drain_pending_relay() {
        if let Ok(signature) = client.send_transaction(&pending.tx_bytes).await {
            let slot_submitted = state.chain.current_blockhash().map_or(0, |(_, slot)| slot);
            tracing::info!(
                %signature,
                slot_submitted,
                correlation = ?pending.correlation,
                "relayed transaction submitted — awaiting confirmation"
            );
            state
                .chain
                .track_awaiting_confirmation(signature, slot_submitted, pending.correlation);
        } else {
            // Best-effort by design (OFS-4300), and silent until now: a
            // caller saw `queued: true` and never learned the submission
            // failed. The bytes are not requeued because the signed
            // blockhash expires anyway.
            tracing::warn!(
                correlation = ?pending.correlation,
                "relayed transaction was not accepted by any endpoint and will NOT be retried — \
                 the caller must resubmit against a fresher blockhash"
            );
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

/// How often an opted-in node pins content it has learned about, and
/// how often it challenges a peer.
///
/// `[PROPOSED — NEEDS SIGN-OFF]` ten minutes. Both are background chores
/// against a reward epoch measured in days, so the cadence only bounds
/// how promptly a new attachment becomes pinned and how many samples a
/// peer's score rests on. Faster would add load to every operator's IPFS
/// daemon and to every peer's RPC for no better measurement.
const PINNING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// How long the gossip event log keeps an event before dropping it.
///
/// `[PROPOSED — NEEDS SIGN-OFF]` seven days, and the number is bounded
/// from below by two separate requirements rather than chosen for
/// roundness:
///
/// - Replay protection needs 24 hours (`docs/architecture.md`), so a
///   week is a wide margin rather than a close call. Past the window a
///   re-gossiped event is applied again instead of recognised as a
///   duplicate, which every registry's own idempotence absorbs — but it
///   should stay a theoretical path, not a routine one.
/// - Recovery: a peer away for less than a week is caught up from the
///   log. Longer outages bootstrap from a snapshot, which is what
///   snapshots are for.
///
/// Bounded from above by the fact that this column family holds every
/// event's full payload beside the record that event already produced,
/// so on a busy node it is the largest thing on disk and the only one
/// that grows purely with time.
const GOSSIP_LOG_RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// How often the gossip log is swept. Hourly: the window is a week, so
/// the sweep cadence only bounds how far past the window the log drifts,
/// and a scan of a large column family is not something to do often.
const GOSSIP_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Advertises this node as publicly reachable, once, at startup.
///
/// # Why the node signs this itself
///
/// Every other registration in OFS-1500 is signed by whoever provides the
/// service, and this is no different — the node is the provider. It holds
/// its own key, so it can make the claim under its own identity rather
/// than needing an operator to run a separate registration step that they
/// would forget and that would then silently expire.
///
/// # What the claim is and is not
///
/// It says "this URL reaches me". It is not a promise of uptime, and a
/// consumer should treat it as a candidate to try rather than a
/// guarantee — which is what `openfiat-app`'s node picker already does,
/// measuring each one with a real request instead of trusting the list.
fn advertise_public_api<S: KvStore + 'static>(state: &NodeState<S>, keypair: &Keypair, url: &str) {
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_types::{InfrastructureService, ServiceId, ServiceType};

    let Ok(provider) = openfiat_network::identity::peer_id_from_public_key(&keypair.public_key())
    else {
        return;
    };

    // Derived from the identity, not random: re-registering on every
    // restart must update the same record rather than accumulating one
    // dead entry per boot.
    let service_id = ServiceId::new(format!("node-{}", hex_peer(&provider)));

    let registration = Registration {
        service_id,
        service_type: ServiceType::Infrastructure(InfrastructureService::PublicApiNode),
        provider: provider.clone(),
        provider_public_key: keypair.public_key(),
        endpoints: vec![url.to_string()],
        supported_ofs: vec![8200],
        region: None,
        capabilities: Vec::new(),
        // No pricing: a public API node is not charging for this, and a
        // price without a payout wallet is refused anyway.
        pricing: None,
        payout_wallet: None,
        timestamp: openfiat_types::Timestamp::now(),
    };

    let signed = SignedRegistration::sign(registration, keypair);
    match state.services.apply_registration(signed.clone()) {
        Ok(_) => {
            let gossip_bytes =
                wire::to_bytes(&signed).expect("SignedRegistration always serializes");
            crate::dispatch::originate(
                state,
                openfiat_registry::protocol::EVENT_REGISTERED,
                openfiat_registry::protocol::OFS_SPEC,
                openfiat_types::Priority::Reputation,
                gossip_bytes,
            );
            tracing::info!(url, "advertised this node as publicly reachable");
        }
        Err(err) => tracing::warn!(?err, url, "could not advertise this node"),
    }
}

fn hex_peer(peer: &openfiat_types::PeerId) -> String {
    peer.as_bytes()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One sweep of the record families that expire.
///
/// Only oracle, risk and session records. Every one of them carries an
/// `expires_at`, is already refused by readers once past it, and — the
/// part that matters — has no aggregate derived by scanning its history.
///
/// The marketplace records are deliberately not here. `ReputationView`
/// and `CounterpartyView` both answer by scanning `settlements.all()` on
/// every call, so deleting an old settlement would silently reduce a
/// wallet's trade count and reputation, and two nodes with different
/// retention would give different answers to the same question while both
/// looked authoritative. Worse, it is unrecoverable: once the settlements
/// are gone the figure cannot be recomputed. Those families can only be
/// pruned after their aggregates are materialised — see #108.
fn poll_expired_records<S: KvStore + 'static>(state: &NodeState<S>) {
    let now = openfiat_types::Timestamp::now();
    let dropped = state.oracles.prune_expired(now)
        + state.risk.prune_expired(now)
        + state.sessions.prune_expired(now);
    if dropped > 0 {
        tracing::info!(dropped, "pruned long-expired records");
    }
}

/// One sweep of the gossip event log.
fn poll_gossip_pruning<S: KvStore + 'static>(state: &NodeState<S>) {
    let cutoff = openfiat_types::Timestamp::from_millis(
        openfiat_types::Timestamp::now()
            .as_millis()
            .saturating_sub(GOSSIP_LOG_RETENTION.as_millis() as u64),
    );
    let dropped = state.gossip.borrow().store().prune_before(cutoff);
    if dropped > 0 {
        tracing::info!(
            dropped,
            retained_days = GOSSIP_LOG_RETENTION.as_secs() / 86_400,
            "pruned the gossip event log"
        );
    }
}

/// One tick of pinning: hold the content this node has learned about.
///
/// Only the challengeable subset is kept locally — see
/// `openfiat_content::held` for why that bound falls out of what a
/// challenge can decide rather than being a separate policy. Everything
/// else is still pinned through the daemon, where it stays available to
/// IPFS without this node holding a copy.
///
/// A node with no `--ipfs-api-url` never reaches here at all.
async fn poll_pinning<S: KvStore + 'static>(
    state: &NodeState<S>,
    client: &dyn openfiat_content::PinningClient,
    retention: openfiat_content::Retention,
) {
    let now = openfiat_types::Timestamp::now();
    let attachments = state.attachments.all();

    // What this node is committed to holding: verifiable content inside
    // its own retention window, which for an archival node is everything
    // and for a rolling node is its recent slice.
    let wanted: Vec<openfiat_crypto::Cid> = attachments
        .iter()
        .filter(|a| a.cid.is_verifiable() && retention.keeps(a.created_at, now))
        .map(|a| a.cid.clone())
        .collect();

    // Release what fell out of the window first, so a node that shrank
    // its retention frees disk on the next tick rather than only after it
    // has finished pinning everything new.
    let dropped = state.held_content.evict_outside(&wanted);
    if dropped > 0 {
        tracing::info!(
            dropped,
            retention = %retention.describe(),
            "evicted content outside the retention window"
        );
    }

    let mut kept = 0usize;

    for cid in wanted {
        if state.held_content.holds(&cid) {
            continue;
        }
        // Pin first: the point of opting in is that the content survives
        // the original uploader unpinning it, and that depends on the
        // daemon, not on the copy kept here.
        if let Err(err) = client.pin(&cid).await {
            tracing::warn!(%err, cid = %cid, "could not pin content");
            continue;
        }
        match client.fetch(&cid).await {
            Ok(bytes) => {
                if state.held_content.keep(&cid, &bytes) {
                    kept += 1;
                }
            }
            // Pinned but not yet retrievable is ordinary right after a
            // pin, so this is not a warning: the next tick tries again.
            Err(err) => tracing::debug!(%err, cid = %cid, "pinned but not yet readable"),
        }
    }

    if kept > 0 {
        tracing::info!(
            kept,
            held = state.held_content.count(),
            "pinned new content"
        );
    }
}

/// One tick of challenging: ask a peer to prove it holds something.
///
/// The result feeds `LivenessLedger::observe_content_served`, which is
/// what the reward premium reads. Only a verified answer counts — see
/// `openfiat_content::challenge` for why "failed" and "could not be
/// decided" have to stay distinct, and why a node is never penalised for
/// the second.
async fn poll_content_challenges<S: KvStore + 'static>(state: &NodeState<S>) {
    let now = openfiat_types::Timestamp::now();
    let attachments = state.attachments.all();
    let pool = openfiat_content::challengeable(&attachments, now);
    if pool.is_empty() {
        return;
    }

    // Seeded by the clock so a peer cannot hold one lucky file and pass
    // for ever, and so two nodes challenging the same peer do not both
    // ask about the same content.
    let seed = now.as_millis();
    let Some(cid) = openfiat_content::challenge::select(&pool, seed) else {
        return;
    };

    // Peers are reachable because they registered an endpoint (OFS-1500).
    // No new discovery mechanism is needed for this, and a node that
    // registered nothing is simply not challenged — it also cannot be
    // paid, since `compute` requires `registered`.
    let services = state.services.all();
    if services.is_empty() {
        return;
    }
    let service = &services[(seed as usize) % services.len()];
    let Some(endpoint) = service.endpoints.first() else {
        return;
    };

    let outcome = challenge_peer(endpoint, cid).await;
    match outcome {
        openfiat_content::ChallengeOutcome::Served => {
            state
                .reward_observations
                .borrow_mut()
                .observe_content_served(&state.reward_params, &service.provider, now);
            tracing::debug!(cid = %cid, endpoint, "peer proved it holds content");
        }
        openfiat_content::ChallengeOutcome::Failed => {
            tracing::debug!(cid = %cid, endpoint, "peer did not serve content");
        }
        // Never recorded either way. Scoring this as a failure would
        // penalise a peer for a limit in our own verification.
        openfiat_content::ChallengeOutcome::Undecidable => {}
    }
}

/// Issues one challenge over a peer's public JSON-RPC endpoint.
pub(crate) async fn challenge_peer(
    endpoint: &str,
    cid: &openfiat_crypto::Cid,
) -> openfiat_content::ChallengeOutcome {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getHeldContent",
        "params": { "cid": cid.as_str() },
    });

    let response = reqwest::Client::new()
        .post(format!("{}/rpc", endpoint.trim_end_matches('/')))
        .json(&body)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await;

    let Ok(response) = response else {
        return openfiat_content::judge(cid, None);
    };
    let Ok(parsed) = response.json::<Value>().await else {
        return openfiat_content::judge(cid, None);
    };
    let encoded = parsed
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.as_str());
    let Some(encoded) = encoded else {
        return openfiat_content::judge(cid, None);
    };
    // A peer choosing what to return is exactly the case this is built
    // for, so a malformed answer is a failure rather than an error worth
    // reporting separately.
    match BASE64.decode(encoded) {
        Ok(bytes) => openfiat_content::judge(cid, Some(&bytes)),
        Err(_) => openfiat_content::judge(cid, None),
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
             --solana-rpc-url to take part in governance tallying."
        );
    }
}

/// How often a node with no checkpoint looks for a snapshot to bootstrap
/// from.
///
/// Thirty seconds because this only runs *until* the node has a
/// checkpoint, and what it is waiting on is the first announcement to
/// arrive over gossip after startup — usually seconds. A node that has
/// already imported one never runs this again, so the cadence costs a
/// timer wakeup for the rest of the process's life and nothing else.
const SNAPSHOT_BOOTSTRAP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Writes and announces a snapshot of this node's own state (OFS-1300
/// §11). No-op unless the operator configured both an interval and a
/// public URL — see [`SnapshotConfig::produces`].
///
/// Every failure is reported and contained. A node that cannot produce a
/// snapshot is a node that is not helping others bootstrap; it is not a
/// node that should stop serving, so nothing here is fatal. The one
/// failure worth naming loudly is `Unauthorized`, which means this node
/// is not registered as a snapshot provider and so its announcements
/// would be dropped by every peer — a configuration problem an operator
/// can only fix if they are told about it.
fn poll_snapshot_production<S: KvStore + 'static>(state: &NodeState<S>, config: &SnapshotConfig) {
    if !config.produces() {
        return;
    }
    let store = Rc::clone(&state.store);
    let height = state.gossip.borrow().event_count() as u64;
    let (producer, producer_public_key) = {
        let gossip = state.gossip.borrow();
        (gossip.node.local_peer_id(), gossip.public_key())
    };

    // Checked before serializing anything, not after: an unregistered
    // node's announcement is rejected by its own index and by every peer,
    // so producing first would write a file every interval that nothing
    // can ever fetch — filling the disk with snapshots whose only reader
    // would have to have heard an announcement that was never made.
    if !state.snapshots.is_registered_provider(&producer) {
        eprintln!(
            "openfiat-node: snapshot production is configured, but this node is not registered \
             as an Infrastructure/SnapshotProvider service, so its announcements would be \
             rejected by every peer. Register the service (sendServiceRegistration) to start \
             producing; nothing is being written until then."
        );
        return;
    }

    let produced = match openfiat_snapshot::producer::produce(
        &store,
        crate::state::SNAPSHOT_COLUMN_FAMILIES,
        config,
        height,
        producer,
        producer_public_key,
    ) {
        Ok(produced) => produced,
        Err(error) => {
            eprintln!("openfiat-node: could not write a snapshot: {error}");
            return;
        }
    };

    // Signed, applied locally, then gossiped — the same order every
    // `sendX` handler uses, so a rejection is a real error here rather
    // than a silent drop at every peer.
    let metadata = produced.metadata.clone();
    let signature = {
        let bytes = openfiat_serialization::json::to_bytes(&metadata)
            .expect("SnapshotMetadata always serializes");
        state.gossip.borrow().sign(&bytes)
    };
    let signed = openfiat_snapshot::events::SignedSnapshotAnnounce {
        metadata: metadata.clone(),
        signature,
    };
    let gossip_bytes = wire::to_bytes(&signed).expect("SignedSnapshotAnnounce always serializes");
    match state.snapshots.apply_announce(signed) {
        Ok(id) => {
            crate::dispatch::originate(
                state,
                openfiat_snapshot::protocol::EVENT_ANNOUNCED,
                openfiat_snapshot::protocol::OFS_SPEC,
                openfiat_types::Priority::Snapshot,
                gossip_bytes,
            );
            println!(
                "openfiat-node: announced snapshot {} at height {} ({} bytes) from {}",
                id.as_str(),
                metadata.height,
                metadata.size_bytes,
                produced.path.display()
            );
        }
        Err(error) => eprintln!(
            "openfiat-node: wrote snapshot {} but could not announce it: {error}. \
             A node must be registered as an Infrastructure/SnapshotProvider service \
             before its announcements are accepted by any peer.",
            metadata.id.as_str()
        ),
    }
}

/// Bootstraps this node from the best snapshot it knows of, if it has
/// never imported one (OFS-1300 §13-17).
///
/// Runs only while `checkpoint_height()` is `None`: once a snapshot has
/// landed, this node's state comes from gossip, and re-importing would
/// overwrite newer state with older — which `SnapshotIndex::import`
/// refuses anyway, but there is no reason to ask.
///
/// The chosen snapshot is the highest-height one this node has a
/// *verified* announcement for. Every announcement in that index already
/// passed a signature check and a service-registry authorization check,
/// and the bytes fetched against it are verified again before anything is
/// written, so choosing by height alone is safe: the worst a hostile
/// producer can do by claiming an enormous height is waste this node one
/// download that then fails to verify.
async fn poll_snapshot_bootstrap<S: KvStore + 'static>(
    state: &NodeState<S>,
    client: &reqwest::Client,
) {
    if state.snapshots.checkpoint_height().is_some() {
        return;
    }
    let Some(candidate) = state.snapshots.latest() else {
        return;
    };

    match openfiat_snapshot::fetch::fetch_and_import(&state.snapshots, client, &candidate.id).await
    {
        Ok(restored) => println!(
            "openfiat-node: bootstrapped from snapshot {} at height {} — {restored} state \
             entries imported, gossip catch-up resumes from there instead of full replay",
            candidate.id.as_str(),
            candidate.height
        ),
        // Loud, and specifically not fatal: the next tick tries again,
        // and a snapshot that fails verification has changed nothing.
        // Starting without state is recoverable; starting with someone
        // else's forged state is not.
        Err(error) => eprintln!(
            "openfiat-node: refused snapshot {} from {:?}: {error}. Continuing without a \
             checkpoint; will retry.",
            candidate.id.as_str(),
            candidate.producer
        ),
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
            let advertise_keypair = Keypair::from_seed(network.keypair.seed());
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
            // A node that produces nothing still ticks this arm, which
            // returns immediately — cheaper than the conditional-arm
            // gymnastics `tokio::select!` would otherwise need, and the
            // interval is an hour by default.
            let snapshot_config = network.snapshot;
            let pinning_client = network
                .ipfs_api_url
                .as_deref()
                .map(openfiat_content::KuboClient::new);
            let retention = network.retention;
            if let Some(url) = network.public_rpc_url.as_deref() {
                advertise_public_api(&state, &advertise_keypair, url);
            }
            let mut snapshot_produce = tokio::time::interval(
                snapshot_config
                    .interval
                    .unwrap_or(openfiat_snapshot::config::DEFAULT_INTERVAL),
            );
            let mut snapshot_bootstrap = tokio::time::interval(SNAPSHOT_BOOTSTRAP_INTERVAL);
            let mut pinning = tokio::time::interval(PINNING_INTERVAL);
            let mut gossip_sweep = tokio::time::interval(GOSSIP_SWEEP_INTERVAL);
            let snapshot_client = reqwest::Client::new();

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
                    _ = snapshot_produce.tick() => {
                        poll_snapshot_production(&state, &snapshot_config);
                    }
                    _ = snapshot_bootstrap.tick() => {
                        poll_snapshot_bootstrap(&state, &snapshot_client).await;
                    }
                    _ = gossip_sweep.tick() => {
                        poll_gossip_pruning(&state);
                        poll_expired_records(&state);
                    }
                    _ = pinning.tick() => {
                        // Challenging runs whether or not this node pins:
                        // measuring peers is a service to the network, and
                        // a node that stores nothing can still check who
                        // does.
                        if let Some(client) = pinning_client.as_ref() {
                            poll_pinning(&state, client, retention).await;
                        }
                        poll_content_challenges(&state).await;
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
    /// make their node count it. Before, the owning program was an operator
    /// setting, and this account would have been believed.
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

    /// The whole retrievability loop, over a real HTTP server speaking
    /// the real wire format.
    ///
    /// This is what makes the reward premium more than a constant: a
    /// node's `pinning_bps` is decided by whether an exchange like this
    /// one succeeded, so if this path is wrong every operator is scored
    /// identically and the multiplier means nothing.
    mod content_challenges {
        use super::*;
        use axum::Json;
        use axum::routing::post;
        use openfiat_content::ChallengeOutcome;
        use serde_json::json;

        /// The bytes `PROBE_CID` names — uploaded to Filebase, fetched
        /// back from ipfs.io, and reproduced by an independent CID
        /// implementation.
        const PROBE_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";
        const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

        /// Stands up a node-shaped `/rpc` endpoint returning `body` as
        /// `getHeldContent`'s result. Returns its base URL.
        async fn peer_serving(body: serde_json::Value) -> String {
            let app = axum::Router::new().route(
                "/rpc",
                post(move || {
                    let body = body.clone();
                    async move { Json(json!({ "jsonrpc": "2.0", "id": 1, "result": body })) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            format!("http://{addr}")
        }

        fn probe_cid() -> openfiat_crypto::Cid {
            openfiat_crypto::Cid::parse(PROBE_CID).unwrap()
        }

        #[tokio::test]
        async fn a_peer_returning_the_named_content_passes() {
            let endpoint = peer_serving(json!({ "content": BASE64.encode(PROBE_CONTENT) })).await;
            assert_eq!(
                challenge_peer(&endpoint, &probe_cid()).await,
                ChallengeOutcome::Served
            );
        }

        #[tokio::test]
        async fn a_peer_returning_different_bytes_fails() {
            // The case the whole mechanism exists for: a node that wants
            // the premium without doing the storage. It cannot produce
            // the preimage of a hash, so anything it invents fails here.
            let endpoint =
                peer_serving(json!({ "content": BASE64.encode(b"invented content") })).await;
            assert_eq!(
                challenge_peer(&endpoint, &probe_cid()).await,
                ChallengeOutcome::Failed
            );
        }

        #[tokio::test]
        async fn a_peer_holding_nothing_fails_rather_than_erroring() {
            // `content: null` is the honest answer from a node that did
            // not opt in, and it must score as "did not serve" — not as
            // a protocol error that stops the challenger's loop.
            let endpoint = peer_serving(json!({ "content": null })).await;
            assert_eq!(
                challenge_peer(&endpoint, &probe_cid()).await,
                ChallengeOutcome::Failed
            );
        }

        #[tokio::test]
        async fn malformed_base64_is_a_failure_not_a_panic() {
            let endpoint = peer_serving(json!({ "content": "!!!! not base64 !!!!" })).await;
            assert_eq!(
                challenge_peer(&endpoint, &probe_cid()).await,
                ChallengeOutcome::Failed
            );
        }

        #[tokio::test]
        async fn an_unreachable_peer_fails_without_hanging_the_loop() {
            // Port 1 is reserved and nothing listens there.
            assert_eq!(
                challenge_peer("http://127.0.0.1:1", &probe_cid()).await,
                ChallengeOutcome::Failed
            );
        }
    }
}
