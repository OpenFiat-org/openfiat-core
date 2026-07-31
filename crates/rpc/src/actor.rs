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
use openfiat_network::behaviour::OpenFiatBehaviourEvent;
use openfiat_network::{Multiaddr, Node};
use openfiat_network::{SwarmEvent, request_response};
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
    /// Whether this node holds and serves protocol content.
    ///
    /// **On by default**, and the inversion is deliberate. This used to
    /// require `--ipfs-api-url` and a separate Kubo daemon, which meant
    /// almost nobody would do it — and a durability guarantee nobody opts
    /// into is not a guarantee. Now that a node serves content itself
    /// (see `openfiat_content::bitswap`), the cost is disk rather than a
    /// second process, and the operator who genuinely cannot spare it is
    /// the one who knows.
    ///
    /// `false`, from `--no-content-serving`, means the node stores
    /// nothing, answers no challenge, and earns the reduced
    /// `pinning_absent_bps` share rather than `pinning_serving_bps`.
    pub serve_content: bool,
    /// Whether this node announces the content it holds on the public
    /// IPFS DHT.
    ///
    /// **On by default when the node serves content at all**, because
    /// serving without announcing is a guarantee only the peers this node
    /// already talks to can use. A public gateway — which is what an
    /// interface actually fetches attachments through — finds a provider
    /// through the DHT or not at all, so this is the difference between
    /// durable in principle and durable in fact.
    ///
    /// `false`, from `--no-content-announce`, means the node still holds
    /// content, still serves it over bitswap to peers that ask, and never
    /// publishes a record. What publishing discloses is this node's peer
    /// id and dialable addresses, globally and to strangers — that is the
    /// point of it, and an operator who would rather their node were
    /// reachable only by peers that already know it is entitled to say
    /// so. It costs them nothing in rewards: a challenge arrives over the
    /// registered JSON-RPC endpoint, not the DHT.
    pub announce_content: bool,
    /// Where this node fetches a block no peer has yet.
    ///
    /// Bitswap moves blocks between peers that have them; it does not
    /// create the first copy. Content enters the network through a
    /// pinning service, so the first OpenFiat node to want a CID has to
    /// get it from the wider IPFS network — see
    /// `openfiat_content::gateway` for why an untrusted gateway is a
    /// sound way to do that and what it does not fix.
    pub content_gateway: String,
    /// An existing IPFS daemon to pin through as well, from
    /// `--ipfs-api-url`.
    ///
    /// No longer how a node serves content — it serves in process now —
    /// but an operator who already runs a Kubo cluster can still have
    /// protocol content pinned into it, which puts the content somewhere
    /// this node's own retention window does not govern.
    pub ipfs_api_url: Option<String>,
    /// How long this node keeps the content it pins. Defaults to a
    /// bounded rolling window — running a node should not be an
    /// open-ended storage commitment.
    pub retention: openfiat_content::Retention,
    /// Addresses peers should dial to reach this node, when the bound
    /// address is not one of them.
    ///
    /// A node behind NAT, in a container, or on a cloud host with a mapped
    /// public IP binds a private address and cannot discover its public
    /// one — by construction only something on the far side can observe
    /// it. So the operator declares it, and peer discovery announces it
    /// ahead of the bound addresses so a dialer reaches the node on the
    /// first attempt rather than after timing out on `10.0.0.5`.
    ///
    /// Empty is right for a node genuinely on a public interface: its
    /// bound address already is its public one.
    pub external_addresses: Vec<Multiaddr>,
    /// Where this node's earnings should be paid, if not to its own
    /// identity address.
    ///
    /// A node cannot start without a keypair, so it always has an address
    /// it demonstrably controls, and that is the default. This exists for
    /// the operator who would rather not accrue earnings to a key living
    /// unencrypted on a server.
    pub payout_wallet: Option<String>,
    /// A region this node declares itself to be in, for clients that
    /// prefer a nearby one. Self-declared and unverified — see #173.
    pub region: Option<String>,
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
            external_addresses: Vec::new(),
            payout_wallet: None,
            region: None,
            serve_content: true,
            // Off for a test node, unlike the shipped default: joining the
            // public IPFS DHT dials four hostnames on the open internet,
            // which a unit test must not do.
            announce_content: false,
            content_gateway: openfiat_content::DEFAULT_GATEWAY.to_string(),
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
    let event = gossip.node.next_event().await;

    // One swarm, two protocols, routed by the envelope's own spec number.
    //
    // This is the wiring peer discovery never had. `DiscoveryService` was
    // fully implemented and converged five nodes in its own test, and no
    // running node ever constructed one — so nodes announced no address
    // and learned no peer they were not handed statically. The reason it
    // was not a one-line dependency addition is right here: each service
    // used to own a `Node`, only one thing can drive a swarm's event loop,
    // and gossip had it. Whichever service did not own the swarm received
    // nothing, for ever, while looking entirely healthy.
    //
    // Envelope messages have exactly one owner and are dispatched by
    // `ofs_spec`, which is the field OFNP §20 defines for precisely this.
    // Everything else — connections opening and closing, addresses being
    // bound, a peer reporting what it observed — belongs to both, so both
    // see it.
    match event {
        SwarmEvent::Behaviour(OpenFiatBehaviourEvent::Envelope(
            request_response::Event::Message { peer, message, .. },
        )) => {
            let is_discovery = match &message {
                request_response::Message::Request { request, .. } => {
                    openfiat_discovery::DiscoveryService::<std::rc::Rc<S>>::owns(request)
                }
                request_response::Message::Response { response, .. } => {
                    openfiat_discovery::DiscoveryService::<std::rc::Rc<S>>::owns(response)
                }
            };
            if is_discovery {
                state
                    .discovery
                    .borrow_mut()
                    .handle_message(peer, message, &mut gossip.node);
            } else {
                gossip.handle_message(peer, message);
            }
        }
        other => {
            state
                .discovery
                .borrow_mut()
                .handle_lifecycle(&other, &mut gossip.node);
            gossip.handle_lifecycle(&other);
        }
    }

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
                    Some(("dispute", tail)) => {
                        // `dispute:<id>` or `dispute:<id>:<case address>`.
                        // The address is what lets this node read what the
                        // chain decided rather than assume it; without one
                        // the execution is recorded and the verdict is not.
                        let (id, case_account) = match tail.split_once(':') {
                            Some((id, account)) => (id.to_string(), Some(account.to_string())),
                            None => (tail, None),
                        };
                        let outcome = match &case_account {
                            Some(address) => read_dispute_outcome(client, address).await,
                            None => {
                                tracing::warn!(
                                    dispute = %id,
                                    "a dispute execution confirmed with no case address in its \
                                     correlation tag, so this node cannot read what the chain \
                                     decided; the case stays awaiting a verdict"
                                );
                                None
                            }
                        };
                        let _ = state.disputes.apply_onchain_execution(
                            &openfiat_disputes::DisputeId::new(id),
                            awaiting.signature.clone(),
                            outcome,
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
/// What this node can actually do, derived from how it is running.
///
/// Not hand-written and not empty. A registration is how one node tells
/// the rest of the network what it is for, and the previous version
/// declared nothing at all — so every public node looked identical, and a
/// client choosing between them had nothing to choose on. Every entry
/// here is a fact about this process at this moment, so it cannot drift
/// from what the operator configured: change the flag, restart, and the
/// registration changes with it.
///
/// Self-declared, like every registration field. A node claiming
/// `chain:rpc` is making a claim, not proving one — but it is a claim
/// that fails visibly, because a `GossipOnly` node cannot answer
/// `getLatestBlockhash` with a fresh blockhash and anyone can ask.
fn declared_capabilities(network: &NetworkConfig) -> Vec<String> {
    let mut capabilities = vec![
        if network.chain_mode.is_rpc_connected() {
            "chain:rpc".to_string()
        } else {
            "chain:gossip".to_string()
        },
        format!("retention:{}", network.retention.describe()),
    ];
    if network.serve_content {
        // The one an interface actually acts on: a node that serves
        // content is one a browser can fetch an attachment from.
        capabilities.push("content:serving".to_string());
    }
    if network.snapshot.produces() {
        capabilities.push("snapshots:producing".to_string());
    }
    capabilities
}

/// Where this node's earnings should be paid.
///
/// Defaults to the node's own identity address rather than to nothing. A
/// node cannot start without a keypair — it is the same key that signs
/// every event it originates — so "this node has no wallet" was never
/// true, and registering `payout_wallet: None` meant a node doing real
/// work had nowhere for its share to go. `ServicePricing` is refused
/// without a payout wallet, so it also meant a node could never charge
/// for anything.
///
/// An operator who would rather not accrue earnings to a key that lives
/// unencrypted on a server passes `--payout-wallet` and names a cold one.
/// That is the right default to *offer* and the wrong one to *assume*:
/// defaulting to a wallet the node cannot prove it controls would send
/// payments nowhere recoverable.
fn payout_wallet(network: &NetworkConfig, keypair: &Keypair) -> String {
    network
        .payout_wallet
        .clone()
        .unwrap_or_else(|| bs58::encode(keypair.public_key().as_bytes()).into_string())
}

/// Everything this node will say about itself, decided once from the
/// configuration as given.
struct PublicApiAdvert {
    capabilities: Vec<String>,
    region: Option<String>,
    payout_wallet: String,
    url: String,
}

fn advertise_public_api<S: KvStore + 'static>(
    state: &NodeState<S>,
    advert: &PublicApiAdvert,
    keypair: &Keypair,
) {
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
        endpoints: vec![advert.url.clone()],
        // Everything this node speaks, from the one place that composes
        // every domain crate — not the single `8200` this used to
        // declare, which described the RPC surface and none of the
        // protocols reached through it.
        supported_ofs: crate::state::SUPPORTED_OFS.to_vec(),
        // Self-declared and optional. A node on a laptop has no useful
        // region to state, and inventing one from an IP lookup would be
        // guessing about the operator on their behalf.
        region: advert.region.clone(),
        capabilities: advert.capabilities.clone(),
        // Still no pricing: serving the public API is not something this
        // node charges for. The payout wallet below is for the reward
        // share it earns by running, which is a different thing.
        pricing: None,
        payout_wallet: Some(advert.payout_wallet.clone()),
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
            tracing::info!(
                url = %advert.url,
                capabilities = ?advert.capabilities,
                payout_wallet = %advert.payout_wallet,
                "advertised this node as publicly reachable"
            );
        }
        Err(err) => tracing::warn!(?err, url = %advert.url, "could not advertise this node"),
    }
}

/// What this node says about itself as a snapshot provider, decided once
/// from the configuration as given — the same discipline
/// [`PublicApiAdvert`] follows, and for the same reason.
struct SnapshotProviderAdvert {
    region: Option<String>,
    payout_wallet: String,
    capabilities: Vec<String>,
}

/// Puts this node on file as an `Infrastructure/SnapshotProvider`
/// (OFS-1300 §5/§24), serving snapshots at `endpoints`.
///
/// **Why the node does this itself.** The registry is what authorizes a
/// snapshot announcement: `SnapshotIndex::apply_announce` rejects one from
/// a producer that is not registered, and so does every peer. A node that
/// produced snapshots but never registered would announce into universal
/// rejection — production would be "on by default" and do nothing, which
/// is the failure mode `--snapshot-public-url` already demonstrated. On by
/// default has to include being allowed to.
///
/// **Why here rather than at startup.** The registration names where the
/// snapshots can be fetched, and at startup this node does not yet know:
/// its addresses arrive as libp2p binds and as peers report back. Running
/// it each production cycle also refreshes `last_health_update`, so the
/// record survives `expire_stale` for as long as the node keeps producing
/// — and lapses on its own once it stops, which is the honest outcome.
fn register_as_snapshot_provider<S: KvStore + 'static>(
    state: &NodeState<S>,
    advert: &SnapshotProviderAdvert,
    keypair: &Keypair,
    endpoints: &[openfiat_snapshot::SnapshotLocation],
) {
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_types::{InfrastructureService, ServiceId, ServiceType};

    let Ok(provider) = openfiat_network::identity::peer_id_from_public_key(&keypair.public_key())
    else {
        return;
    };

    let registration = Registration {
        // Derived from the identity, like the public-API record's, so a
        // restart updates one entry instead of leaving a dead one per boot.
        service_id: ServiceId::new(format!("snapshot-{}", hex_peer(&provider))),
        service_type: ServiceType::Infrastructure(InfrastructureService::SnapshotProvider),
        provider: provider.clone(),
        provider_public_key: keypair.public_key(),
        endpoints: endpoints.iter().map(|url| url.to_string()).collect(),
        supported_ofs: vec![openfiat_snapshot::protocol::OFS_SPEC],
        region: advert.region.clone(),
        capabilities: advert.capabilities.clone(),
        // Serving a snapshot is not something this node charges for.
        pricing: None,
        payout_wallet: Some(advert.payout_wallet.clone()),
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
        }
        Err(err) => tracing::warn!(?err, "could not register this node as a snapshot provider"),
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

/// How many blocks one tick will fetch from a gateway.
///
/// A node that has just joined, or one whose retention window just
/// widened, can be missing a great deal at once. Fetching it all in one
/// tick would open that many requests to someone else's gateway and hold
/// that much in memory; spreading it over ticks costs only time, and the
/// content is not urgent — a challenge that arrives before the fetch is
/// answered by whoever already has it.
///
/// A budget rather than a count of attachments, because one attachment is
/// now one block or forty of them depending on its size. It is checked
/// between DAGs and not inside one: a partially fetched DAG is not a
/// smaller file, so a walk that started finishes.
const MAX_GATEWAY_BLOCKS_PER_TICK: usize = 64;

/// What the chain decided about a dispute case, read from the case
/// account itself.
///
/// `None` for every reason a node might not know: the account could not
/// be fetched, it is not a `DisputeCase`, it is owned by some other
/// program, or the case is genuinely still running. All of those mean the
/// same thing to the caller — this node cannot state a verdict — and none
/// of them is a reason to invent one.
async fn read_dispute_outcome(
    client: &dyn ChainClient,
    case_account: &str,
) -> Option<openfiat_disputes::Resolution> {
    let (owner, data) = match client.get_account(case_account).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            tracing::warn!(%case_account, "dispute case account not found on chain");
            return None;
        }
        Err(err) => {
            tracing::warn!(%case_account, %err, "could not read the dispute case account");
            return None;
        }
    };
    match crate::onchain_dispute::decode_outcome(&owner, &data) {
        Ok(outcome) => outcome,
        Err(err) => {
            tracing::warn!(%case_account, ?err, "dispute case account did not decode");
            None
        }
    }
}

/// One tick of content serving: hold what this node is committed to,
/// release what it no longer is, and go and get what is missing.
///
/// # What a node is committed to
///
/// The content referenced by *accepted* attachment records inside this
/// node's retention window. Not everything it has ever seen: an
/// attachment needs a settlement and a settlement needs real escrow, so
/// this set is bounded by the network's actual trading volume rather than
/// by anyone's willingness to publish CIDs at it.
///
/// All of it, including the chunked files a node used to skip. A DAG is
/// fetched block by block and every block is checked against its own CID
/// — see `openfiat_content::dag` — so nothing here is taken on trust that
/// was not before. What a *challenge* can decide is unchanged and still
/// narrower; `poll_content_challenges` draws from `challengeable`.
///
/// # Peers first, gateway second
///
/// A CID missing on this tick is asked of every connected peer. A CID
/// still missing on the *next* tick — having given the peers a round to
/// answer — is fetched from a public gateway, which is how content that
/// no OpenFiat node holds yet enters the network at all. The one-tick
/// delay is what keeps the gateway a fallback rather than the first
/// thing every node reaches for.
async fn poll_content<S: KvStore + 'static>(
    state: &NodeState<S>,
    control: &libp2p_stream::Control,
    gateway: &openfiat_content::GatewayFetcher,
    retention: openfiat_content::Retention,
    announce: bool,
) {
    let now = openfiat_types::Timestamp::now();
    let attachments = state.attachments.all();

    // What this node is committed to holding: verifiable content inside
    // its own retention window, which for an archival node is everything
    // and for a rolling node is its recent slice.
    let wanted: Vec<openfiat_crypto::Cid> = attachments
        .iter()
        .filter(|a| retention.keeps(a.created_at, now))
        .map(|a| a.cid.clone())
        .collect();

    // Release what fell out of the window first, so a node that shrank
    // its retention frees disk on the next tick rather than only after it
    // has finished fetching everything new.
    let dropped = state.held_content.evict_outside(&wanted);
    if dropped > 0 {
        tracing::info!(
            dropped,
            retention = %retention.describe(),
            "evicted content outside the retention window"
        );
    }

    // What is still needed, block by block. For a raw CID that is the CID
    // itself; for a chunked file it is the root until the root arrives and
    // then whichever leaves that root turned out to name, which is why
    // this cannot be a `holds` check — a node with the root and none of
    // the leaves holds nothing anyone can read.
    let mut seen = std::collections::HashSet::new();
    let missing: Vec<openfiat_crypto::Cid> = wanted
        .iter()
        .flat_map(|root| state.held_content.missing_blocks(root))
        .filter(|cid| seen.insert(cid.as_str().to_string()))
        .collect();

    // Anything asked for last tick and still absent has had its round
    // with the peers. Computed before the want list is replaced, because
    // that is the whole distinction between the two paths.
    let unanswered: Vec<openfiat_crypto::Cid> = {
        let previous = state.content_wants.borrow();
        missing
            .iter()
            .filter(|cid| previous.contains(cid.as_str()))
            .cloned()
            .collect()
    };
    state
        .content_wants
        .replace(missing.iter().map(|cid| cid.as_str().to_string()).collect());

    if !missing.is_empty() {
        let request = openfiat_content::bitswap::wantlist(&missing);
        // Every connected peer, not a chosen few. A wantlist is small,
        // the peers are already connected, and a node that asked only
        // its favourite would fail to find content the others have.
        let peers: Vec<_> = state
            .gossip
            .borrow()
            .node
            .swarm
            .connected_peers()
            .copied()
            .collect();
        for peer in peers {
            openfiat_content::bitswap::spawn_send(control.clone(), peer, request.clone());
        }
    }

    let mut kept = 0usize;
    let mut fetched = 0usize;
    for cid in &unanswered {
        if fetched >= MAX_GATEWAY_BLOCKS_PER_TICK {
            break;
        }
        // The whole DAG under this CID, not one block of it. A gateway
        // round trip per tick per block would take forty ticks to assemble
        // one receipt, during which the node holds a file it cannot serve.
        match openfiat_content::dag::fetch(gateway, cid).await {
            Ok(blocks) => {
                fetched += blocks.len();
                for (block, bytes) in blocks {
                    if state.held_content.keep(&block, &bytes) {
                        state.content_wants.borrow_mut().remove(block.as_str());
                        kept += 1;
                    }
                }
            }
            // Ordinary rather than alarming: the content may genuinely
            // be gone, or the gateway may be having a moment. Either way
            // the next tick tries again, and a peer may answer first.
            Err(err) => tracing::debug!(%err, cid = %cid, "no copy from the gateway yet"),
        }
    }

    if kept > 0 {
        tracing::info!(
            kept,
            held = state.held_content.count(),
            "fetched content from a gateway"
        );
    }

    if announce {
        announce_held_content(state, &wanted);
    }
}

/// Publishes, on the public IPFS DHT, what this node can actually serve.
///
/// # Roots, not every block
///
/// A chunked attachment is forty blocks and one record. Announcing each
/// block would be forty DHT publishes per attachment, each a query across
/// twenty peers, for content nobody looks up by leaf: a client resolves
/// the CID in the record, which is the root. Having found this node it
/// asks the same bitswap session for the children, because that is what
/// every bitswap implementation does once a peer has answered — so the
/// leaves are reachable without ever being advertised.
///
/// # Only what is complete
///
/// A node holding a root and half its leaves would answer a provider
/// query and then fail to serve, which is worse for the client than not
/// being found: it spends a connection and a round trip discovering it.
///
/// # Withdrawing is local, and that is not a gap
///
/// There is no message in the protocol for retracting a provider record;
/// they expire on their own. Stopping is what ends the republishing that
/// would otherwise keep renewing a claim about evicted content.
///
/// Returns how many records were newly announced and how many withdrawn,
/// which is what a caller — and a test — can actually observe: the query
/// itself is fire-and-forget and its outcome is the DHT's business.
fn announce_held_content<S: KvStore + 'static>(
    state: &NodeState<S>,
    wanted: &[openfiat_crypto::Cid],
) -> (usize, usize) {
    let complete: std::collections::HashSet<String> = wanted
        .iter()
        .filter(|cid| state.held_content.missing_blocks(cid).is_empty())
        .map(|cid| cid.as_str().to_string())
        .collect();

    let mut provided = state.content_provided.borrow_mut();
    if complete == *provided {
        return (0, 0);
    }

    let mut gossip = state.gossip.borrow_mut();
    let mut announced = 0usize;
    for spelling in complete.difference(&provided) {
        // Every string in this set was a `Cid` a moment ago.
        let cid = openfiat_crypto::Cid::parse(spelling).expect("held content is parsed content");
        if gossip.node.start_providing(&cid.multihash()) {
            announced += 1;
        }
    }
    let mut withdrawn = 0usize;
    for spelling in provided.difference(&complete) {
        let cid = openfiat_crypto::Cid::parse(spelling).expect("held content is parsed content");
        gossip.node.stop_providing(&cid.multihash());
        withdrawn += 1;
    }

    *provided = complete;
    if announced > 0 || withdrawn > 0 {
        tracing::info!(
            announced,
            withdrawn,
            providing = provided.len(),
            "announced content on the IPFS DHT"
        );
    }
    (announced, withdrawn)
}

/// A bitswap message from `peer`, handled on the actor thread.
///
/// Two jobs, in this order. Blocks this node asked for are kept, which is
/// how content reaches a node from its peers rather than from a gateway.
/// Then whatever the peer wanted is answered from what this node holds.
///
/// Doing the keeping first is not arbitrary: a peer that sends a block
/// and asks for it in the same message gets the answer it deserves.
fn on_bitswap<S: KvStore + 'static>(
    state: &NodeState<S>,
    peer: libp2p_identity::PeerId,
    message: openfiat_content::bitswap::Message,
    control: &libp2p_stream::Control,
) {
    for (cid, bytes) in &message.blocks {
        // Only what this node asked for. `keep` already refuses bytes
        // that are not what the CID names, so an unsolicited block
        // cannot be *wrong* — but a peer pushing correct blocks for
        // content this node never wanted is a disk-filling primitive,
        // and this is the line that closes it.
        if !state.content_wants.borrow().contains(cid.as_str()) {
            continue;
        }
        if state.held_content.keep(cid, bytes) {
            state.content_wants.borrow_mut().remove(cid.as_str());
            tracing::info!(%cid, %peer, "a peer supplied content this node was missing");
        }
    }

    let reply = openfiat_content::bitswap::respond(&*state.held_content, &message);
    openfiat_content::bitswap::spawn_send(control.clone(), peer, reply);
}

/// One tick of pinning into an operator's own IPFS daemon.
///
/// Not how a node serves content — it serves in process now — but an
/// operator who already runs a Kubo cluster can have protocol content
/// pinned into it as well, which puts a copy somewhere this node's own
/// retention window does not govern.
async fn poll_daemon_pinning<S: KvStore + 'static>(
    state: &NodeState<S>,
    client: &dyn openfiat_content::PinningClient,
    retention: openfiat_content::Retention,
) {
    let now = openfiat_types::Timestamp::now();
    for attachment in state.attachments.all() {
        if !retention.keeps(attachment.created_at, now) {
            continue;
        }
        if let Err(err) = client.pin(&attachment.cid).await {
            tracing::warn!(%err, cid = %attachment.cid, "could not pin into the operator's daemon");
        }
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

/// Every address this node is willing to *act* on believing it is
/// reachable at, most likely to work first.
///
/// Two sources, because neither alone is complete. Discovery holds what
/// the operator declared (`--external-addr`) followed by what libp2p
/// actually bound, which is the answer for a host whose interface address
/// is its real one. Gossip additionally holds identify's `observed_addr` —
/// what a *peer* saw the connection arrive from — which is the only thing
/// that can see through a NAT, and so the only source that answers for the
/// deployment this used to need a flag for.
///
/// `corroborated_addresses`, deliberately, and not `reachable_addresses`:
/// an observed address is one peer's unverified claim, and what this
/// function feeds is the location an honest producer signs and gossips to
/// the whole cluster. See `GossipService::corroborated_addresses` for what
/// a single reporter could otherwise aim every joining node at.
///
/// Duplicates are expected and harmless: both sources report the bound
/// addresses, and `openfiat_snapshot::reachable` collapses them by host.
fn reachable_addresses<S: KvStore + 'static>(state: &NodeState<S>) -> Vec<Multiaddr> {
    state
        .discovery
        .borrow()
        .announced_addresses()
        .iter()
        .filter_map(|address| address.parse().ok())
        .chain(state.gossip.borrow().corroborated_addresses())
        .collect()
}

/// Writes and announces a snapshot of this node's own state (OFS-1300
/// §11). On by default — see [`SnapshotConfig::produces`], which now turns
/// only on the interval.
///
/// Every failure is reported and contained. A node that cannot produce a
/// snapshot is a node that is not helping others bootstrap; it is not a
/// node that should stop serving, so nothing here is fatal.
fn poll_snapshot_production<S: KvStore + 'static>(
    state: &NodeState<S>,
    config: &SnapshotConfig,
    advert: &SnapshotProviderAdvert,
    keypair: &Keypair,
) {
    if !config.produces() {
        return;
    }
    let store = Rc::clone(&state.store);
    // A snapshot says when its state is current as of, and this node must
    // have genuinely observed that moment rather than invent a number. A
    // GossipOnly node learns slots over the chain bridge, so this is not a
    // requirement to hold an RPC connection — only to have heard from the
    // network at all.
    let Some((_, slot)) = state.chain.current_blockhash() else {
        eprintln!(
            "openfiat-node: no snapshot written — this node has not observed a Solana slot yet, \
             so it cannot say what its state is current as of. An RpcConnected node learns one on \
             its first chain poll; a GossipOnly node learns one from the first peer carrying a \
             BlockhashAnnounced."
        );
        return;
    };
    let (producer, producer_public_key) = {
        let gossip = state.gossip.borrow();
        (gossip.node.local_peer_id(), gossip.public_key())
    };

    // Recomputed every cycle rather than resolved once at startup. The
    // set grows as the node learns about itself — a listen address is
    // known within milliseconds of binding, but the public address behind
    // a NAT only arrives once a peer has connected and identify has run.
    // A snapshot must be announced under the addresses that were true when
    // it was written, not the ones known at boot.
    let base_urls = config.locations(&reachable_addresses(state));
    if base_urls.is_empty() {
        eprintln!(
            "openfiat-node: no snapshot written — this node has not learned an address peers \
             could fetch one from yet. It normally learns one within seconds of its first peer \
             connection; if this node is behind a reverse proxy, pass --snapshot-public-url."
        );
        return;
    }

    // Before serializing anything: an announcement from a producer the
    // registry does not vouch for is rejected by this node's own index and
    // by every peer, so producing first would write a file every interval
    // that nothing can ever fetch.
    register_as_snapshot_provider(state, advert, keypair, &base_urls);
    if !state.snapshots.is_registered_provider(&producer) {
        // Reached only if the registration this node just made for itself
        // did not land — a corrupt store, or a service id already held by
        // another identity. Neither is something an operator can be
        // expected to infer from silence.
        eprintln!(
            "openfiat-node: no snapshot written — this node could not register itself as an \
             Infrastructure/SnapshotProvider service, so its announcements would be rejected by \
             every peer. Check the preceding registry warning."
        );
        return;
    }

    let produced = match openfiat_snapshot::producer::produce(
        &store,
        crate::state::SNAPSHOT_COLUMN_FAMILIES,
        config,
        &base_urls,
        slot,
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
                metadata.slot,
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
/// Runs only while `checkpoint_slot()` is `None`: once a snapshot has
/// landed, this node's state comes from gossip, and re-importing would
/// overwrite newer state with older — which `SnapshotIndex::import`
/// refuses anyway, but there is no reason to ask.
///
/// Candidates are every *verified* announcement this node holds, highest
/// height first, and they are tried in turn until one imports. Every
/// announcement in that index already passed a signature check and a
/// service-registry authorization check, and the bytes fetched against it
/// are verified again before anything is written, so ordering by height
/// alone is safe: the worst a hostile producer can do by claiming an
/// enormous height is waste this node one download that then fails to
/// verify.
///
/// Trying more than one is the point. A single unreachable or corrupt
/// producer at the top of the list used to stall bootstrap indefinitely —
/// every thirty seconds this node would ask the same dead mirror the same
/// question — while a perfectly good snapshot one height lower sat
/// unfetched. Falling through costs at most a few failed requests, all of
/// them bounded by `fetch::download`'s own timeout and size cap.
async fn poll_snapshot_bootstrap<S: KvStore + 'static>(
    state: &NodeState<S>,
    client: &reqwest::Client,
) {
    if state.snapshots.checkpoint_slot().is_some() {
        return;
    }
    let mut candidates = state.snapshots.all();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.slot));

    for candidate in candidates {
        match openfiat_snapshot::fetch::fetch_and_import(&state.snapshots, client, &candidate.id)
            .await
        {
            Ok(restored) => {
                println!(
                    "openfiat-node: bootstrapped from snapshot {} at height {} — {restored} \
                     state entries imported, gossip catch-up resumes from there instead of \
                     full replay",
                    candidate.id.as_str(),
                    candidate.slot
                );
                return;
            }
            // Loud, and specifically not fatal: the next candidate is
            // tried, the next tick tries again from the top, and a
            // snapshot that fails verification has changed nothing.
            // Starting without state is recoverable; starting with someone
            // else's forged state is not.
            Err(error) => eprintln!(
                "openfiat-node: refused snapshot {} from {:?}: {error}. Trying the next \
                 announced snapshot; continuing without a checkpoint until one verifies.",
                candidate.id.as_str(),
                candidate.producer
            ),
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
            let advertise_keypair = Keypair::from_seed(network.keypair.seed());
            // Decided here, from the configuration exactly as the operator
            // gave it, before `NodeState::new` consumes any of it. Reading
            // these later would mean reading whatever survived the move,
            // which is how a registration ends up describing something
            // other than the node that is running.
            let advertisement = network.public_rpc_url.clone().map(|url| PublicApiAdvert {
                capabilities: declared_capabilities(&network),
                region: network.region.clone(),
                payout_wallet: payout_wallet(&network, &advertise_keypair),
                url,
            });
            let snapshot_advert = SnapshotProviderAdvert {
                region: network.region.clone(),
                payout_wallet: payout_wallet(&network, &advertise_keypair),
                // What a snapshot from this node *contains*, which is not
                // the same question as whether it verifies. An archival
                // node ships its whole history and a rolling one its
                // window, so a node fetching for a specific height needs
                // to be able to prefer a provider whose retention covers
                // it. Declared here so that choice is possible without
                // changing the announcement shape again.
                capabilities: vec![format!("retention:{}", network.retention.describe())],
            };
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
                network.snapshot.trusted_providers.clone(),
            );
            {
                // Declared before listening, so the first peer to connect
                // is already told the right address. A node that announced
                // only its bound address for the first few seconds would
                // hand out an undialable one to exactly the peers that
                // arrive at startup.
                let mut discovery = state.discovery.borrow_mut();
                for address in &network.external_addresses {
                    discovery.add_external_address(address.to_string());
                }
                if !network.external_addresses.is_empty() {
                    tracing::info!(
                        addresses = ?network.external_addresses,
                        "announcing operator-declared external addresses"
                    );
                }
            }
            {
                let mut gossip = state.gossip.borrow_mut();
                // The same declarations, told to the swarm as well as to
                // peer discovery. A provider record carries the addresses
                // the swarm has confirmed, so a node that published
                // without these would be telling the IPFS network it has
                // content and giving nobody a way to fetch it.
                for address in &network.external_addresses {
                    gossip.node.announce_address(address.clone());
                }
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
            // Before the loop starts: the registry record should exist by
            // the time this node begins answering, not a tick later.
            if let Some(advertisement) = &advertisement {
                advertise_public_api(&state, advertisement, &advertise_keypair);
            }
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

            // Content serving, on unless the operator turned it off. The
            // control is taken before the swarm goes into the loop below
            // and is independent of it, so reading a peer's stream never
            // needs the borrow every RPC handler is holding.
            let content_gateway = openfiat_content::GatewayFetcher::new(&network.content_gateway);
            let (content_tx, mut content_rx) = mpsc::unbounded_channel();
            let content_control = network.serve_content.then(|| {
                let mut control = state.gossip.borrow().node.content_control();
                openfiat_content::bitswap::spawn_inbound(&mut control, content_tx)
                    .expect("nothing else can already be serving bitswap on this node");
                control
            });
            // Joining the public IPFS DHT, which is what makes the content
            // this node serves findable by anyone who has not heard of
            // OpenFiat. Only when it serves content at all — a node with
            // nothing to provide would publish an empty claim and answer
            // no queries, which is a routing table's worth of memory for
            // nothing.
            let announce_content = network.serve_content && network.announce_content;
            if announce_content {
                let bootstrappers = state.gossip.borrow_mut().node.join_content_routing();
                if bootstrappers == 0 {
                    tracing::warn!(
                        "no IPFS DHT bootstrapper resolved; this node's content will not be \
                         findable outside its own peers"
                    );
                } else {
                    tracing::info!(bootstrappers, "joined the public IPFS DHT");
                }
            }
            tracing::info!(
                serving = network.serve_content,
                announcing = announce_content,
                retention = %retention.describe(),
                "content serving"
            );
            // One interval *after* startup, not immediately. `interval`
            // fires its first tick at once, which would snapshot the store
            // this node booted with — for a fresh node, an empty one, at
            // height zero, announced to the cluster as something to
            // bootstrap from. It also fires before the node has heard from
            // any peer, so it would have had no address to announce a
            // location under either.
            let produce_every = snapshot_config
                .interval
                .unwrap_or(openfiat_snapshot::config::DEFAULT_INTERVAL);
            let mut snapshot_produce = tokio::time::interval_at(
                tokio::time::Instant::now() + produce_every,
                produce_every,
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
                        poll_snapshot_production(
                            &state,
                            &snapshot_config,
                            &snapshot_advert,
                            &advertise_keypair,
                        );
                    }
                    _ = snapshot_bootstrap.tick() => {
                        poll_snapshot_bootstrap(&state, &snapshot_client).await;
                    }
                    _ = gossip_sweep.tick() => {
                        poll_gossip_pruning(&state);
                        poll_expired_records(&state);
                    }
                    // Only polled when this node serves content: a
                    // `None` control means no accept loop was started, so
                    // nothing will ever arrive on this channel.
                    Some((peer, message)) = content_rx.recv(), if content_control.is_some() => {
                        on_bitswap(&state, peer, message, content_control.as_ref().expect("guarded"));
                    }
                    _ = pinning.tick() => {
                        if let Some(control) = content_control.as_ref() {
                            poll_content(
                                &state,
                                control,
                                &content_gateway,
                                retention,
                                announce_content,
                            )
                            .await;
                        }
                        if let Some(client) = pinning_client.as_ref() {
                            poll_daemon_pinning(&state, client, retention).await;
                        }
                        // Challenging runs whether or not this node holds
                        // anything: measuring peers is a service to the
                        // network, and a node that stores nothing can
                        // still check who does.
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

    /// The probe file this project genuinely uploaded to IPFS, with the
    /// CID the provider returned for it.
    const PROBE_CID: &str = "bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva";
    const PROBE_CONTENT: &[u8] = b"openfiat ipfs probe 1785426891\n";

    fn block_message(cid: &str, content: &[u8]) -> openfiat_content::bitswap::Message {
        openfiat_content::bitswap::Message {
            blocks: vec![(openfiat_crypto::Cid::parse(cid).unwrap(), content.to_vec())],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_block_this_node_never_asked_for_is_refused_rather_than_stored() {
        // The disk-filling case. `keep` already refuses bytes that are
        // not what the CID names, so a pushed block cannot be *wrong* —
        // but a peer pushing correct blocks this node never wanted, for
        // as long as it likes, is how a node runs out of disk without
        // anything looking like an attack.
        let state = NodeState::new_for_test(MemoryStore::new());
        let control = state.gossip.borrow().node.content_control();
        let cid = openfiat_crypto::Cid::parse(PROBE_CID).unwrap();

        on_bitswap(
            &state,
            libp2p_identity::PeerId::random(),
            block_message(PROBE_CID, PROBE_CONTENT),
            &control,
        );

        assert!(
            !state.held_content.holds(&cid),
            "a node must not store content it never asked for, however valid"
        );
    }

    #[tokio::test]
    async fn a_block_this_node_asked_for_is_kept_and_stops_being_wanted() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let control = state.gossip.borrow().node.content_control();
        let cid = openfiat_crypto::Cid::parse(PROBE_CID).unwrap();
        state
            .content_wants
            .borrow_mut()
            .insert(PROBE_CID.to_string());

        on_bitswap(
            &state,
            libp2p_identity::PeerId::random(),
            block_message(PROBE_CID, PROBE_CONTENT),
            &control,
        );

        assert!(state.held_content.holds(&cid));
        assert!(
            !state.content_wants.borrow().contains(PROBE_CID),
            "content that arrived must stop being asked for, or every peer \
             is asked for it again on every tick for ever"
        );
    }

    #[tokio::test]
    async fn a_dag_pb_block_a_peer_supplies_is_kept_like_any_other() {
        // What #161 changed. This node used to refuse every block of a
        // chunked file, so an attachment over 256 KiB was held by nobody
        // once its pinning service stopped paying. `0a 02 08 01` is the
        // unixfs empty directory — a real dag-pb block IPFS has served
        // since 2015, under the CID below.
        const DAG_PB_CID: &str = "bafybeiczsscdsbs7ffqz55asqdf3smv6klcw3gofszvwlyarci47bgf354";
        let block: &[u8] = &[0x0a, 0x02, 0x08, 0x01];

        let state = NodeState::new_for_test(MemoryStore::new());
        let control = state.gossip.borrow().node.content_control();
        let cid = openfiat_crypto::Cid::parse(DAG_PB_CID).unwrap();
        assert!(!cid.is_verifiable(), "a dag-pb CID, not a raw one");
        state
            .content_wants
            .borrow_mut()
            .insert(DAG_PB_CID.to_string());

        on_bitswap(
            &state,
            libp2p_identity::PeerId::random(),
            block_message(DAG_PB_CID, block),
            &control,
        );

        assert_eq!(state.held_content.get(&cid).as_deref(), Some(block));
        assert!(
            state.held_content.missing_blocks(&cid).is_empty(),
            "a node with no links is a complete DAG"
        );
    }

    /// Announcing on the public IPFS DHT — the thing that makes content
    /// this node holds findable by a gateway, a browser, or anyone who
    /// never heard of OpenFiat.
    ///
    /// These run against a real swarm with an empty routing table, which
    /// is what a node has before it bootstraps: `start_providing` records
    /// the claim locally and the query to publish it finds nobody. That
    /// is exactly the boundary worth testing here — *what* this node
    /// decides to announce is ours, whether the query reaches Amsterdam
    /// is not.
    mod dht_announcements {
        use super::*;

        /// A dag-pb node linking to `children`, and its CID.
        fn chunked(children: &[&openfiat_crypto::Cid]) -> (openfiat_crypto::Cid, Vec<u8>) {
            let mut block = Vec::new();
            for child in children {
                let hash = child.to_binary();
                let mut link = vec![0x0a, hash.len() as u8];
                link.extend_from_slice(&hash);
                block.push(0x12);
                block.push(link.len() as u8);
                block.extend_from_slice(&link);
            }
            block.extend_from_slice(&[0x0a, 0x02, 0x08, 0x02]);
            let mut binary = vec![0x01, 0x70, 0x12, 0x20];
            binary.extend_from_slice(&openfiat_crypto::hash::sha256(&block));
            (openfiat_crypto::Cid::from_binary(&binary).unwrap(), block)
        }

        #[tokio::test]
        async fn content_this_node_holds_is_announced_once_and_not_again() {
            let state = NodeState::new_for_test(MemoryStore::new());
            let cid = openfiat_crypto::Cid::parse(PROBE_CID).unwrap();
            assert!(state.held_content.keep(&cid, PROBE_CONTENT));

            assert_eq!(
                announce_held_content(&state, std::slice::from_ref(&cid)),
                (1, 0),
                "a node serving content nobody can find is the whole problem"
            );

            // A second tick must not re-issue the claim: libp2p republishes
            // on its own schedule, and re-announcing every tick would be a
            // DHT query every pinning interval per attachment held.
            assert_eq!(
                announce_held_content(&state, std::slice::from_ref(&cid)),
                (0, 0)
            );
            assert_eq!(state.content_provided.borrow().len(), 1);
        }

        #[tokio::test]
        async fn a_chunked_file_is_announced_only_once_every_block_is_here() {
            // Announcing a root whose leaves are still missing would send
            // a client a connection and a round trip to discover this node
            // cannot serve what it advertised — worse for them than not
            // being found at all.
            let leaf = openfiat_crypto::Cid::parse(PROBE_CID).unwrap();
            let (root, root_block) = chunked(&[&leaf]);

            let state = NodeState::new_for_test(MemoryStore::new());
            assert!(state.held_content.keep(&root, &root_block));

            assert_eq!(
                announce_held_content(&state, std::slice::from_ref(&root)),
                (0, 0),
                "the root is here and the file is not"
            );

            assert!(state.held_content.keep(&leaf, PROBE_CONTENT));
            assert_eq!(
                announce_held_content(&state, std::slice::from_ref(&root)),
                (1, 0)
            );
            assert!(state.content_provided.borrow().contains(root.as_str()));
        }

        #[tokio::test]
        async fn content_that_fell_out_of_the_window_stops_being_announced() {
            // Otherwise the node keeps renewing a claim about content it
            // evicted, and every renewal points clients at a node that
            // will answer DontHave.
            let state = NodeState::new_for_test(MemoryStore::new());
            let cid = openfiat_crypto::Cid::parse(PROBE_CID).unwrap();
            assert!(state.held_content.keep(&cid, PROBE_CONTENT));
            assert_eq!(
                announce_held_content(&state, std::slice::from_ref(&cid)),
                (1, 0)
            );

            state.held_content.evict_outside(&[]);
            assert_eq!(announce_held_content(&state, &[]), (0, 1));
            assert!(state.content_provided.borrow().is_empty());
        }

        #[test]
        fn the_dht_key_is_the_multihash_the_ipfs_network_looks_up() {
            // Not the CID. A record filed under the full CID is findable
            // only by a client that spelled the content the same way, and
            // the two codecs are two spellings of the same bytes.
            let cid = openfiat_crypto::Cid::parse(PROBE_CID).unwrap();
            assert_eq!(cid.multihash(), cid.to_binary()[2..].to_vec());
            assert_ne!(cid.multihash(), cid.to_binary());
        }
    }

    #[tokio::test]
    async fn a_peer_answering_a_want_with_the_wrong_bytes_supplies_nothing() {
        // The bytes are re-addressed on decode, so they would arrive
        // under their own CID rather than the wanted one — but this
        // asserts the outcome at the layer that stores, since that is
        // where a mistake would become a challenge this node fails.
        let state = NodeState::new_for_test(MemoryStore::new());
        let control = state.gossip.borrow().node.content_control();
        let cid = openfiat_crypto::Cid::parse(PROBE_CID).unwrap();
        state
            .content_wants
            .borrow_mut()
            .insert(PROBE_CID.to_string());

        on_bitswap(
            &state,
            libp2p_identity::PeerId::random(),
            block_message(PROBE_CID, b"substituted by a dishonest peer"),
            &control,
        );

        assert!(!state.held_content.holds(&cid));
        assert!(
            state.content_wants.borrow().contains(PROBE_CID),
            "a failed answer must leave the want standing so another peer is asked"
        );
    }

    #[test]
    fn a_node_declares_what_it_is_actually_running() {
        // Every public node used to register with an empty capability
        // list, so they were indistinguishable and a client choosing
        // between them had nothing to choose on.
        let mut config = NetworkConfig::for_test();
        config.serve_content = true;
        let gossip_only = declared_capabilities(&config);
        assert!(gossip_only.contains(&"chain:gossip".to_string()));
        assert!(gossip_only.contains(&"content:serving".to_string()));
        assert!(
            gossip_only.iter().any(|c| c.starts_with("retention:")),
            "how long a node keeps content is what a client needs to know \
             before relying on it for old evidence: {gossip_only:?}"
        );

        // Turning a thing off has to change what the node claims, or the
        // claim is decoration.
        config.serve_content = false;
        assert!(!declared_capabilities(&config).contains(&"content:serving".to_string()));
    }

    #[test]
    fn a_node_registers_a_payout_wallet_it_controls() {
        // "This node has no wallet" was never true — it cannot start
        // without one, and it is the same key that signs every event.
        let config = NetworkConfig::for_test();
        let keypair = Keypair::from_seed(config.keypair.seed());
        let derived = payout_wallet(&config, &keypair);
        assert_eq!(
            derived,
            bs58::encode(keypair.public_key().as_bytes()).into_string(),
            "the default must be an address this node demonstrably controls"
        );
        assert!(!derived.is_empty());
    }

    #[test]
    fn an_operator_can_send_earnings_somewhere_colder() {
        let mut config = NetworkConfig::for_test();
        let cold = "4oiCmGrMRL4m4RJsRX6F7nCDeEqoiKLYm5hsDcLFvAJB";
        config.payout_wallet = Some(cold.to_string());
        let keypair = Keypair::from_seed(config.keypair.seed());
        assert_eq!(payout_wallet(&config, &keypair), cold);
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
                    endpoints: vec!["https://gw.example.com/deliver".to_string()],
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
            &[("https://gw.example.com/deliver".to_string(), id.clone())]
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

    /// Bootstrapping from the cluster, when the best-looking snapshot is
    /// not the one that works.
    mod snapshot_bootstrap {
        use super::*;
        use openfiat_registry::{Registration, SignedRegistration};
        use openfiat_snapshot::events::SignedSnapshotAnnounce;
        use openfiat_snapshot::location::SnapshotLocation;
        use openfiat_snapshot::record::{CompressionMethod, SnapshotId, SnapshotMetadata};
        use openfiat_types::{InfrastructureService, ServiceId, ServiceType, Timestamp};

        /// The one column family these snapshots carry. Real domain state
        /// with a real query behind it, and one of
        /// `SNAPSHOT_COLUMN_FAMILIES`.
        const CARRIED: &str = "registry_services";

        fn provider(state: &NodeState<MemoryStore>) -> Keypair {
            let keypair = Keypair::from_seed([21u8; 32]);
            state
                .services
                .apply_registration(SignedRegistration::sign(
                    Registration {
                        service_id: ServiceId::new("snapshot-provider"),
                        service_type: ServiceType::Infrastructure(
                            InfrastructureService::SnapshotProvider,
                        ),
                        provider: openfiat_network::identity::peer_id_from_public_key(
                            &keypair.public_key(),
                        )
                        .unwrap(),
                        provider_public_key: keypair.public_key(),
                        endpoints: vec![],
                        supported_ofs: vec![1300],
                        region: None,
                        capabilities: vec![],
                        pricing: None,
                        payout_wallet: None,
                        timestamp: Timestamp::now(),
                    },
                    &keypair,
                ))
                .unwrap();
            keypair
        }

        /// A snapshot blob carrying one registry entry, so an import is
        /// visible as state rather than only as a checkpoint.
        fn blob() -> Vec<u8> {
            let source = MemoryStore::new();
            source
                .put(CARRIED, b"svc-from-snapshot", b"payload")
                .unwrap();
            openfiat_snapshot::state::serialize(&source, &[CARRIED]).unwrap()
        }

        /// Announces `id` at `height`, fetchable at `location`, through the
        /// real signed path — so nothing here reaches `import` by a route a
        /// gossiped announcement could not take.
        fn announce(
            state: &NodeState<MemoryStore>,
            keypair: &Keypair,
            id: &str,
            slot: u64,
            location: &str,
            bytes: &[u8],
        ) -> SnapshotId {
            let metadata = SnapshotMetadata {
                id: SnapshotId::new(id),
                snapshot_version: 1,
                protocol_version: openfiat_snapshot::protocol::SUPPORTED_PROTOCOL_VERSION,
                slot,
                created_at: Timestamp::now(),
                state_root: openfiat_snapshot::codec::state_root(bytes),
                size_bytes: bytes.len() as u64,
                compression: CompressionMethod::None,
                locations: vec![SnapshotLocation::parse(location).unwrap()],
                producer: openfiat_network::identity::peer_id_from_public_key(
                    &keypair.public_key(),
                )
                .unwrap(),
                producer_public_key: keypair.public_key(),
            };
            state
                .snapshots
                .apply_announce(SignedSnapshotAnnounce::sign(metadata, keypair))
                .unwrap()
        }

        /// "On by default" has to include being *allowed* to. The registry
        /// is what authorizes a snapshot announcement, so a node that
        /// produced without registering would announce into rejection by
        /// every peer, itself included — production would be on and do
        /// nothing, which is the state the removed flag already produced
        /// once.
        /// A node that has not observed a slot writes nothing.
        ///
        /// It cannot say what its state is current as of, and a snapshot
        /// whose slot was invented is worse than no snapshot: every peer
        /// orders candidates by that number, so a fabricated one either
        /// buries an honest producer or promotes itself above one.
        #[test]
        fn a_node_that_has_never_seen_a_slot_produces_nothing() {
            let directory = std::env::temp_dir().join(format!(
                "openfiat-actor-noslot-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&directory);

            let keypair = Keypair::from_seed([44u8; 32]);
            let state = NodeState::new(
                Node::new(&keypair).unwrap(),
                MemoryStore::new(),
                Keypair::from_seed([44u8; 32]),
                Vec::new(),
                NodeChainMode::GossipOnly,
                openfiat_snapshot::trust::TrustAnchors::pinned(),
            );
            // Deliberately no `record_blockhash` — this node has heard
            // nothing from the chain, directly or over gossip.
            assert!(state.chain.current_blockhash().is_none());

            let config = openfiat_snapshot::SnapshotConfig {
                directory: directory.clone(),
                // A documentation-range IP, not a `.example` hostname:
                // the registry refuses RFC 2606 reserved suffixes, so a
                // `.example` endpoint would fail self-registration and this
                // test would pass without ever reaching the slot check.
                public_urls: vec![
                    openfiat_snapshot::SnapshotLocation::parse("http://203.0.113.9:7080").unwrap(),
                ],
                ..openfiat_snapshot::SnapshotConfig::default()
            };
            let advert = SnapshotProviderAdvert {
                region: None,
                payout_wallet: "11111111111111111111111111111111".to_string(),
                capabilities: vec!["retention:archival".to_string()],
            };
            poll_snapshot_production(&state, &config, &advert, &keypair);

            assert!(
                std::fs::read_dir(&directory)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    || std::fs::read_dir(&directory).unwrap().count() == 0,
                "a node with no slot must write no snapshot"
            );
            assert!(state.snapshots.latest().is_none(), "and must announce none");
            let _ = std::fs::remove_dir_all(&directory);
        }

        #[test]
        fn a_node_that_was_never_registered_registers_itself_and_produces() {
            let directory = std::env::temp_dir().join(format!(
                "openfiat-actor-produce-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&directory);

            // Built from a key the test holds, because the registration
            // this node makes for itself has to be under the identity it
            // signs its announcements with. Registering some other key
            // would authorize a producer that is not this node — which is
            // exactly what the assertions below would then fail to notice
            // if `NodeState`'s own keypair were used to produce and a
            // second one to register.
            let keypair = Keypair::from_seed([33u8; 32]);
            let state = NodeState::new(
                Node::new(&keypair).unwrap(),
                MemoryStore::new(),
                Keypair::from_seed([33u8; 32]),
                Vec::new(),
                NodeChainMode::GossipOnly,
                openfiat_snapshot::trust::TrustAnchors::pinned(),
            );
            // A snapshot records the slot its state is current as of, so a
            // node that has never seen one refuses to produce. Seeded here
            // rather than stubbed away, because that refusal is a real
            // behaviour with its own test below.
            state
                .chain
                .record_blockhash("11111111111111111111111111111111", 412_000_000);
            let producer =
                openfiat_network::identity::peer_id_from_public_key(&keypair.public_key()).unwrap();
            assert!(
                !state.snapshots.is_registered_provider(&producer),
                "the node starts as nobody's snapshot provider"
            );

            let config = SnapshotConfig {
                directory: directory.clone(),
                // Stands in for a learned address: `new_for_test` never
                // listened, so nothing has been learned to derive from,
                // and this test is about the registration rather than the
                // derivation (covered by `openfiat_snapshot::reachable`).
                public_urls: vec![
                    openfiat_snapshot::SnapshotLocation::parse("http://203.0.113.9:7080").unwrap(),
                ],
                ..SnapshotConfig::default()
            };
            let advert = SnapshotProviderAdvert {
                region: None,
                payout_wallet: "11111111111111111111111111111111".to_string(),
                capabilities: vec!["retention:archival".to_string()],
            };

            poll_snapshot_production(&state, &config, &advert, &keypair);

            assert!(
                state.snapshots.is_registered_provider(&producer),
                "the node must put itself on file rather than wait to be registered by hand"
            );
            let announced = state
                .snapshots
                .latest()
                .expect("a registered producer's announcement lands in its own index");
            assert_eq!(
                announced.locations[0].as_str(),
                format!("http://203.0.113.9:7080/snapshot/{}", announced.id.as_str())
            );
            assert!(
                openfiat_snapshot::producer::snapshot_path(&directory, &announced.id).exists(),
                "the bytes must be on disk before they are announced"
            );

            let _ = std::fs::remove_dir_all(&directory);
        }

        /// The stall this closes: the highest-height announcement came from
        /// a producer that had gone away, and every thirty seconds this
        /// node asked that same dead host the same question while a
        /// perfectly good snapshot one height down sat unfetched.
        #[tokio::test]
        async fn a_dead_producer_at_the_top_does_not_block_the_next_candidate() {
            let directory = std::env::temp_dir().join(format!(
                "openfiat-actor-bootstrap-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).unwrap();

            let bytes = blob();
            std::fs::write(directory.join("snap-usable.snapshot"), &bytes).unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let live = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let _ = axum::serve(
                    listener,
                    openfiat_snapshot::serve::router(directory.clone()),
                )
                .await;
            });

            // The producer is trusted explicitly. This test is about the
            // fall-through when the best-looking snapshot is unreachable,
            // not about who a fresh node believes — that gate is covered
            // in `openfiat_snapshot`'s own suite.
            let anchor =
                bs58::encode(Keypair::from_seed([21u8; 32]).public_key().as_bytes()).into_string();
            let state = NodeState::new_for_test_trusting(
                MemoryStore::new(),
                openfiat_snapshot::trust::TrustAnchors::with_operator(&[anchor]).unwrap(),
            );
            let keypair = provider(&state);
            // Port 1 is reserved and nothing listens there, so the first
            // candidate is refused rather than left to time out.
            announce(
                &state,
                &keypair,
                "snap-gone",
                900,
                "http://127.0.0.1:1/snapshot/snap-gone",
                &bytes,
            );
            let usable = announce(
                &state,
                &keypair,
                "snap-usable",
                100,
                &format!("http://{live}/snapshot/snap-usable"),
                &bytes,
            );

            poll_snapshot_bootstrap(&state, &reqwest::Client::new()).await;

            assert_eq!(
                state.snapshots.checkpoint_slot(),
                Some(100),
                "the lower, reachable snapshot must be what this node bootstrapped from"
            );
            assert_eq!(state.snapshots.get(&usable).unwrap().slot, 100);
            assert_eq!(
                state.store.get(CARRIED, b"svc-from-snapshot").unwrap(),
                Some(b"payload".to_vec()),
                "the fallback candidate's state must actually have landed"
            );
        }
    }
}
