//! Node-level methods — Solana's `getVersion`/`getHealth` equivalents,
//! plus the peers this node has discovered.

use crate::dispatch::{MethodTable, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_storage::KvStore;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct VersionResult {
    pub version: &'static str,
}

/// One peer, as this node's discovery cache knows it.
///
/// Every field is one `PeerRecord` genuinely carries, plus the one fact
/// about a peer that carries a proof — see [`PeerView::served_content`].
/// There is deliberately no uptime percentage and no health score:
/// `successes` and `failures` are this node's own count of exchanges with
/// that peer, which is a real measurement of a real thing, and folding
/// them into a score would present one node's local experience as a
/// network-wide verdict.
///
/// The public key is included because it is what makes a peer's future
/// events verifiable, and it is not a secret — a peer id already encodes
/// it (see `openfiat_network::identity::public_key_from_peer_id`).
#[derive(Debug, Serialize)]
pub struct PeerView {
    pub peer_id: String,
    pub addresses: Vec<String>,
    pub node_version: String,
    pub supported_ofs: Vec<u16>,
    pub roles: Vec<openfiat_types::NodeRole>,
    pub last_seen: openfiat_types::Timestamp,
    pub latency_ms: Option<u32>,
    /// Exchanges with this peer that worked, and that did not, as counted
    /// by this node alone. Two nodes can honestly disagree about both.
    pub successes: u32,
    pub failures: u32,
    /// Whether this node has itself challenged this peer for content and
    /// got back bytes that hash to the CID's own digest, at some point
    /// inside [`PeersResult::content_proof_window`].
    ///
    /// **Observed, not asserted.** Every other claim-shaped field above
    /// (`node_version`, `supported_ofs`, `roles`) is the peer's own word
    /// for itself, and `announced_blockhash` — the connectivity signal
    /// behind the reward — is re-announceable by a node that never
    /// observed anything. This one is not assertable: the only write path
    /// is `actor::poll_content_challenges` recording a
    /// `ChallengeOutcome::Served`, which requires the responding node to
    /// return the bytes a content address names. Nothing on the wire, in
    /// an advertisement, or in a gossip envelope can set it.
    ///
    /// **`false` means unproven, not disproven.** The ledger records
    /// successes only, so "challenged and failed" and "never challenged"
    /// are the same value here and cannot be told apart. A young network,
    /// or a peer that registered no OFS-1500 endpoint to be challenged
    /// through, reads `false` throughout. Do not render it as a negative
    /// verdict.
    ///
    /// The derived reward multiplier is deliberately *not* here — see
    /// [`PeersResult::content_proof_window`].
    pub served_content: bool,
}

/// The window [`PeerView::served_content`] is a statement about.
///
/// Exists because the flag on its own has no freshness bound a caller
/// could infer, and a `true` that silently meant "some time in the last
/// day" would be the more misleading answer. The ledger is per-epoch and
/// the flag is a set-once boolean within one: a peer that proved
/// retrievability in the first minute of the epoch and has been dark
/// since reads identically to one answering right now, until the epoch
/// rolls over and every peer resets to `false`. There is no decay finer
/// than that, and this struct is how a caller sees exactly how stale a
/// `true` may be.
///
/// This is the *in-flight* epoch, unlike `getRewardObservations`, which
/// defaults to the last completed one. The difference is deliberate: a
/// schedule must be computed from an epoch that has stopped changing,
/// whereas an operator asking who is serving now wants the epoch that has
/// not.
#[derive(Debug, Serialize)]
pub struct ContentProofWindow {
    pub epoch: u64,
    pub epoch_start_millis: u64,
    pub epoch_end_millis: u64,
}

#[derive(Debug, Serialize)]
pub struct PeersResult {
    /// This node's own identity, in the form that goes in a multiaddr.
    ///
    /// Reported because an operator publishing an entrypoint needs it and
    /// has nowhere else to get it: `/ip4/<host>/udp/4001/quic-v1/p2p/<this>`
    /// is the string they hand to other operators, and assembling it from
    /// a log line is how it gets typed wrong.
    pub self_peer_id: String,
    pub peers: Vec<PeerView>,
    /// The addresses this node asks peers to dial it at — operator-declared
    /// ones first, then bound ones.
    ///
    /// Worth exposing because "my node announces nothing" was invisible
    /// from outside for as long as it was true, and an operator checking
    /// whether their `--external-addr` took effect has nowhere else to
    /// look.
    pub announced_addresses: Vec<String>,
    /// The epoch every peer's `served_content` covers, and its bounds.
    ///
    /// Named on the result rather than repeated per peer because it is
    /// one window for the whole answer, and a caller that ignores it is
    /// reading a boolean with no notion of when.
    ///
    /// The derived `pinning_bps` multiplier is not published here on
    /// purpose. Turning the fact into an amount needs `RewardParams`,
    /// which this method does not carry and which governance can change;
    /// a bps figure in a peer-discovery view would read as a fixed
    /// property of the peer when it is a property of the parameter set
    /// this node happens to be running. `getRewardObservations` is where
    /// the derived values belong, because it publishes the parameters'
    /// effects for exactly one epoch so that a third party can recompute
    /// the schedule — and it now publishes this one too.
    pub content_proof_window: ContentProofWindow,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getVersion",
        method_fn(
            |_state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<VersionResult, RpcError> {
                Ok(VersionResult {
                    version: crate::version(),
                })
            },
        ),
    );
    table.register(
        "getPeers",
        method_fn(
            |state: &NodeState<S>, _params: serde_json::Value| -> Result<PeersResult, RpcError> {
                let discovery = state.discovery.borrow();
                // The in-flight epoch: what a peer has proved *this*
                // epoch, keyed by the same `PeerId` the discovery cache
                // uses. Both are derived from the peer's Ed25519 key
                // (`identity::peer_id_from_public_key`), and the registry
                // checks a provider's claimed id against its key before a
                // challenge can ever be attributed, so the join is on one
                // identity and not two that happen to look alike.
                let epoch = state
                    .reward_params
                    .epoch_index(openfiat_types::Timestamp::now());
                let (epoch_start, epoch_end) = state.reward_params.epoch_bounds(epoch);
                let proven = state.reward_observations.borrow().epoch(epoch);

                let peers = discovery
                    .cache
                    .all()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|record| PeerView {
                        served_content: proven
                            .get(&record.peer_id)
                            .is_some_and(|live| live.served_content),
                        // The `12D3Koo…` form an operator can paste into
                        // an `--entrypoint`, not the byte array the wire
                        // format uses.
                        peer_id: openfiat_network::identity::readable_peer_id(&record.peer_id)
                            .unwrap_or_default(),
                        addresses: record.addresses,
                        node_version: record.node_version,
                        supported_ofs: record.supported_ofs,
                        roles: record.roles,
                        last_seen: record.last_seen,
                        latency_ms: record.latency_ms,
                        successes: record.successes,
                        failures: record.failures,
                    })
                    .collect();
                Ok(PeersResult {
                    self_peer_id: state.gossip.borrow().node.libp2p_peer_id().to_string(),
                    peers,
                    announced_addresses: discovery.announced_addresses(),
                    content_proof_window: ContentProofWindow {
                        epoch,
                        epoch_start_millis: epoch_start,
                        epoch_end_millis: epoch_end,
                    },
                })
            },
        ),
    );
    table.register(
        "getHealth",
        method_fn(
            |_state: &NodeState<S>, _params: serde_json::Value| -> Result<&'static str, RpcError> {
                Ok("ok")
            },
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_discovery::PeerRecord;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{NodeRole, PeerId, PublicKey, Timestamp};

    fn record(tag: u8) -> PeerRecord {
        PeerRecord::new(
            PeerId::from_bytes(vec![tag; 8]),
            PublicKey::from_bytes([tag; 32]),
            vec!["/ip4/127.0.0.1/udp/4001/quic-v1".to_string()],
            "1.0.0".to_string(),
            vec![1100],
            vec![NodeRole::FullNode],
        )
    }

    fn get_peers(state: &NodeState<MemoryStore>) -> serde_json::Value {
        let mut table = crate::dispatch::MethodTable::new();
        register(&mut table);
        table
            .dispatch(state, "getPeers", serde_json::Value::Null)
            .expect("getPeers takes no params and cannot fail")
    }

    /// The join is the thing under test. A challenge is recorded through
    /// the ledger the challenge loop actually writes to, and the flag has
    /// to arrive on the *right* peer's record — a version that attached
    /// it to every peer, or to none, would pass a shape-only assertion.
    #[test]
    fn a_proven_retrieval_reaches_the_peer_that_earned_it_and_no_other() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let (served, silent) = (record(1), record(2));
        state.discovery.borrow().cache.upsert(&served).unwrap();
        state.discovery.borrow().cache.upsert(&silent).unwrap();

        state
            .reward_observations
            .borrow_mut()
            .observe_content_served(&state.reward_params, &served.peer_id, Timestamp::now());

        let answer = get_peers(&state);
        let flags: Vec<bool> = answer["peers"]
            .as_array()
            .expect("peers is a list")
            .iter()
            .map(|peer| {
                peer["served_content"]
                    .as_bool()
                    .expect("served_content is always present, never omitted when false")
            })
            .collect();

        assert_eq!(flags.len(), 2);
        assert_eq!(
            flags.iter().filter(|proven| **proven).count(),
            1,
            "exactly the one peer that returned the bytes is marked, not both and not neither"
        );
    }

    /// A `true` with no window is a boolean a caller cannot date, and the
    /// window is the only freshness bound there is — the flag does not
    /// decay inside an epoch, it resets when one ends.
    #[test]
    fn the_answer_says_which_epoch_the_content_proof_covers() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let now = Timestamp::now();
        let window = &get_peers(&state)["content_proof_window"];

        let epoch = window["epoch"].as_u64().expect("an epoch index");
        assert_eq!(
            epoch,
            state.reward_params.epoch_index(now),
            "the window is the in-flight epoch, not the last completed one"
        );

        let (start, end) = (
            window["epoch_start_millis"].as_u64().unwrap(),
            window["epoch_end_millis"].as_u64().unwrap(),
        );
        assert!(
            (start..end).contains(&now.as_millis()),
            "a window that does not contain the present moment dates nothing"
        );
    }
}
