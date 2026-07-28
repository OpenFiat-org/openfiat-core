//! Wires the three OFS-4300 events into a node's shared [`GossipService`]:
//! blockhash announcement/dedup (§6) and transaction relay origination/
//! observation (§7). Policy on *what to do* with an incoming relay
//! request (actually submit it via [`crate::RpcChainClient`], or ignore
//! it on a `GossipOnly` node) is composed by the caller through
//! [`ChainBridge::on_relay_requested`] — this type only owns the gossip
//! mechanics, the same separation `openfiat-oracles`' own `OracleService`
//! keeps from `openfiat-registry`.
//!
//! Two ways to use this, matching the two node shapes this workspace
//! already has: [`ChainGossipService`] owns its `GossipService` outright
//! (a chain-bridge-only node); [`ChainBridge::install`] instead takes
//! `&mut GossipService<S>`, for a node — like `openfiat-conformance`'s
//! `FullNode` — that composes many domains onto one shared gossip
//! instance the same way every other registry there attaches itself.

use crate::blockhash::BlockhashCache;
use crate::error::ChainError;
use crate::events::{BlockhashAnnounced, TransactionRelayRequested, TransactionRelayed};
use crate::protocol;
use openfiat_gossip::GossipService;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventId, EventType, Priority, Timestamp};
use std::cell::RefCell;
use std::rc::Rc;

/// This priority enum (OFS-1600 §10) has no dedicated Chain tier yet —
/// `[PROPOSED — NEEDS SIGN-OFF]` treat chain-bridge events as
/// settlement-adjacent, since a stale blockhash or an unrelayed
/// transaction directly blocks OFS-2300 settlement from completing.
const CHAIN_PRIORITY: Priority = Priority::SessionReservationSettlement;

type RelayRequestHandler = Box<dyn FnMut(&TransactionRelayRequested)>;
type RelayConfirmationHandler = Box<dyn FnMut(&TransactionRelayed)>;

/// The chain bridge's gossip-facing state, independent of who owns the
/// underlying [`GossipService`] — see the module doc for the two ways
/// this gets used.
pub struct ChainBridge {
    cache: Rc<RefCell<BlockhashCache>>,
    relay_request_handlers: Rc<RefCell<Vec<RelayRequestHandler>>>,
    relay_confirmation_handlers: Rc<RefCell<Vec<RelayConfirmationHandler>>>,
}

impl ChainBridge {
    /// Registers this bridge's event handler and forward filter on
    /// `gossip` — callable alongside any number of other domains'
    /// `add_event_handler` calls on the same shared instance.
    pub fn install<S: KvStore + 'static>(gossip: &mut GossipService<S>) -> Self {
        let cache = Rc::new(RefCell::new(BlockhashCache::new()));
        let relay_request_handlers: Rc<RefCell<Vec<RelayRequestHandler>>> =
            Rc::new(RefCell::new(Vec::new()));
        let relay_confirmation_handlers: Rc<RefCell<Vec<RelayConfirmationHandler>>> =
            Rc::new(RefCell::new(Vec::new()));

        // Blockhash announcements are deliberately *not* handled here —
        // see the forward filter below. `notify()` (which calls this
        // handler) always runs before `should_forward()` for a received
        // event; if both called `cache.observe()`, the filter's call
        // would always see "already seen" (the handler just consumed
        // the first sighting) and never forward anything, including
        // genuinely new content. One call site per received event, on
        // the filter, is what actually gives `observe()`'s return value
        // meaning. A self-originating node updates its own cache via
        // `announce_blockhash`'s direct call instead, since no filter
        // runs for a self-originated event.
        let handler_requests = Rc::clone(&relay_request_handlers);
        let handler_confirmations = Rc::clone(&relay_confirmation_handlers);
        gossip.add_event_handler(move |event| {
            if event.ofs_spec != protocol::OFS_SPEC {
                return;
            }
            match event.event_type.as_str() {
                protocol::EVENT_TRANSACTION_RELAY_REQUESTED => {
                    if let Ok(requested) =
                        wire::from_bytes::<TransactionRelayRequested>(&event.payload)
                    {
                        for handler in handler_requests.borrow_mut().iter_mut() {
                            handler(&requested);
                        }
                    }
                }
                protocol::EVENT_TRANSACTION_RELAYED => {
                    if let Ok(relayed) = wire::from_bytes::<TransactionRelayed>(&event.payload) {
                        for handler in handler_confirmations.borrow_mut().iter_mut() {
                            handler(&relayed);
                        }
                    }
                }
                _ => {}
            }
        });

        // §6's amplification control: suppress re-forwarding a
        // `BlockhashAnnounced` whose (blockhash, slot) content this node
        // has already seen, regardless of the event's own id/signature.
        let filter_cache = Rc::clone(&cache);
        gossip.add_forward_filter(move |event| {
            if event.ofs_spec != protocol::OFS_SPEC
                || event.event_type.as_str() != protocol::EVENT_BLOCKHASH_ANNOUNCED
            {
                return true;
            }
            match wire::from_bytes::<BlockhashAnnounced>(&event.payload) {
                Ok(announced) => filter_cache
                    .borrow_mut()
                    .observe(&announced.blockhash, announced.slot),
                Err(_) => true, // malformed payload: not this filter's call to make
            }
        });

        Self {
            cache,
            relay_request_handlers,
            relay_confirmation_handlers,
        }
    }

    /// This node's current view of the blockhash to build or forward a
    /// transaction against (OFS-4300 §6) — from gossip on a `GossipOnly`
    /// node, or from this node's own most recent self-announcement on an
    /// `RpcConnected` one (both paths update the same cache).
    pub fn current_blockhash(&self) -> Option<(String, u64)> {
        self.cache
            .borrow()
            .current()
            .map(|(hash, slot)| (hash.to_string(), slot))
    }

    /// Originates a `BlockhashAnnounced` (§6) — callers decide their own
    /// announcement cadence (minimum interval/slot-delta); this method
    /// doesn't rate-limit on their behalf.
    pub fn announce_blockhash<S: KvStore + 'static>(
        &self,
        gossip: &mut GossipService<S>,
        blockhash: &str,
        slot: u64,
    ) -> Result<EventId, ChainError> {
        self.cache.borrow_mut().observe(blockhash, slot);
        let payload = BlockhashAnnounced {
            blockhash: blockhash.to_string(),
            slot,
            observed_at: Timestamp::now(),
        };
        originate(gossip, protocol::EVENT_BLOCKHASH_ANNOUNCED, &payload)
    }

    /// Originates a `TransactionRelayRequested` (§7) — the entry point a
    /// `GossipOnly` node uses to get an already-signed transaction onto
    /// the chain via an RPC-connected peer. `correlation` is an opaque
    /// caller-supplied tag (e.g. a settlement ID) carried through to
    /// whichever peer ends up submitting and confirming it.
    pub fn request_transaction_relay<S: KvStore + 'static>(
        &self,
        gossip: &mut GossipService<S>,
        tx_bytes: Vec<u8>,
        correlation: Option<String>,
    ) -> Result<EventId, ChainError> {
        let payload = TransactionRelayRequested {
            tx_bytes,
            requested_at: Timestamp::now(),
            correlation,
        };
        originate(
            gossip,
            protocol::EVENT_TRANSACTION_RELAY_REQUESTED,
            &payload,
        )
    }

    /// Originates a `TransactionRelayed` confirmation echo (§7) — best
    /// effort, called by whichever RPC-connected peer actually submitted
    /// a relayed transaction and observed it land.
    pub fn announce_relay_confirmation<S: KvStore + 'static>(
        &self,
        gossip: &mut GossipService<S>,
        signature: &str,
        slot_submitted: u64,
    ) -> Result<EventId, ChainError> {
        let payload = TransactionRelayed {
            signature: signature.to_string(),
            slot_submitted,
        };
        originate(gossip, protocol::EVENT_TRANSACTION_RELAYED, &payload)
    }

    /// Registers a callback invoked for every `TransactionRelayRequested`
    /// this node stores (its own, or a peer's, per gossip's usual
    /// self-plus-received semantics) — where an `RpcConnected` node's
    /// NodeState composition wires actual submission via
    /// [`crate::RpcChainClient`].
    pub fn on_relay_requested(&self, handler: impl FnMut(&TransactionRelayRequested) + 'static) {
        self.relay_request_handlers
            .borrow_mut()
            .push(Box::new(handler));
    }

    /// Registers a callback invoked for every `TransactionRelayed`
    /// confirmation this node stores.
    pub fn on_relay_confirmed(&self, handler: impl FnMut(&TransactionRelayed) + 'static) {
        self.relay_confirmation_handlers
            .borrow_mut()
            .push(Box::new(handler));
    }
}

fn originate<S: KvStore + 'static>(
    gossip: &mut GossipService<S>,
    event_name: &str,
    payload: &impl serde::Serialize,
) -> Result<EventId, ChainError> {
    let bytes = wire::to_bytes(payload).map_err(|_| ChainError::MalformedTransaction)?;
    let event_type = EventType::new(event_name)
        .expect("chain bridge event names are valid PascalCase identifiers");
    gossip
        .originate(event_type, protocol::OFS_SPEC, CHAIN_PRIORITY, 8, bytes)
        .map_err(|_| ChainError::ChainUnavailable)
}

/// A chain-bridge-only node: owns its `GossipService` outright (rather
/// than composing onto a shared one — see [`ChainBridge`] for that case).
pub struct ChainGossipService<S> {
    pub gossip: GossipService<S>,
    bridge: ChainBridge,
}

impl<S: KvStore + 'static> ChainGossipService<S> {
    pub fn new(mut gossip: GossipService<S>) -> Self {
        let bridge = ChainBridge::install(&mut gossip);
        Self { gossip, bridge }
    }

    pub fn current_blockhash(&self) -> Option<(String, u64)> {
        self.bridge.current_blockhash()
    }

    pub fn announce_blockhash(
        &mut self,
        blockhash: &str,
        slot: u64,
    ) -> Result<EventId, ChainError> {
        self.bridge
            .announce_blockhash(&mut self.gossip, blockhash, slot)
    }

    pub fn request_transaction_relay(
        &mut self,
        tx_bytes: Vec<u8>,
        correlation: Option<String>,
    ) -> Result<EventId, ChainError> {
        self.bridge
            .request_transaction_relay(&mut self.gossip, tx_bytes, correlation)
    }

    pub fn announce_relay_confirmation(
        &mut self,
        signature: &str,
        slot_submitted: u64,
    ) -> Result<EventId, ChainError> {
        self.bridge
            .announce_relay_confirmation(&mut self.gossip, signature, slot_submitted)
    }

    pub fn on_relay_requested(
        &mut self,
        handler: impl FnMut(&TransactionRelayRequested) + 'static,
    ) {
        self.bridge.on_relay_requested(handler);
    }

    pub fn on_relay_confirmed(&mut self, handler: impl FnMut(&TransactionRelayed) + 'static) {
        self.bridge.on_relay_confirmed(handler);
    }
}
