//! [`ChainState`]: the synchronous slot `openfiat-rpc`'s `NodeState`
//! composes (OFS-4300 §8's `getChainStatus`/`getLatestBlockhash`/
//! `sendTransaction`). RPC dispatch in this workspace is synchronous
//! end to end (see `openfiat-rpc::dispatch::MethodFn`) — the same
//! constraint every other domain's `sendX` handler already lives under
//! (it applies to local state rather than actually reaching the
//! network). `sendTransaction` here queues an already-validated
//! transaction rather than submitting it inline; draining that queue
//! through a real [`crate::ChainClient`] or [`crate::ChainGossipService`]
//! is the async node-composition layer's job, not this crate's.

use crate::blockhash::BlockhashCache;
use crate::error::ChainError;
use crate::mode::NodeChainMode;
use crate::validate::validate_transaction_bytes;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Duration;

/// A relay request not yet submitted to the chain.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRelay {
    pub tx_bytes: Vec<u8>,
    /// Opaque caller-supplied tag (e.g. a settlement ID) — this crate
    /// never interprets it; whichever caller drains the queue and later
    /// observes confirmation (`openfiat-rpc`'s poll loop) does, so its
    /// own domain registries can react to real on-chain finality.
    pub correlation: Option<String>,
}

/// A transaction submitted to the chain but not yet observed as
/// confirmed — tracked so a caller can poll `ChainClient::
/// get_signature_status` until it resolves, rather than treating mere
/// RPC-submission acceptance as confirmation.
#[derive(Debug, Clone, PartialEq)]
pub struct AwaitingConfirmation {
    pub signature: String,
    pub slot_submitted: u64,
    pub correlation: Option<String>,
}

pub struct ChainState {
    mode: NodeChainMode,
    cache: RefCell<BlockhashCache>,
    pending_relay: RefCell<VecDeque<PendingRelay>>,
    awaiting_confirmation: RefCell<Vec<AwaitingConfirmation>>,
}

impl ChainState {
    pub fn new(mode: NodeChainMode) -> Self {
        Self {
            mode,
            cache: RefCell::new(BlockhashCache::new()),
            pending_relay: RefCell::new(VecDeque::new()),
            awaiting_confirmation: RefCell::new(Vec::new()),
        }
    }

    pub fn mode(&self) -> &NodeChainMode {
        &self.mode
    }

    pub fn current_blockhash(&self) -> Option<(String, u64)> {
        self.cache
            .borrow()
            .current()
            .map(|(hash, slot)| (hash.to_string(), slot))
    }

    pub fn current_blockhash_age(&self) -> Option<Duration> {
        self.cache.borrow().current_age()
    }

    /// Records an observed blockhash — called by whichever async driver
    /// feeds this node's blockhash (its own RPC polling loop, or
    /// [`crate::ChainGossipService`]'s event handler).
    pub fn record_blockhash(&self, blockhash: &str, slot: u64) {
        self.cache.borrow_mut().observe(blockhash, slot);
    }

    /// OFS-4300 §7: rejects a malformed payload before ever queuing it.
    /// `correlation` is an opaque tag threaded through to
    /// [`AwaitingConfirmation`] once submitted, for a caller to react to
    /// once real confirmation is observed.
    pub fn enqueue_relay(
        &self,
        tx_bytes: Vec<u8>,
        correlation: Option<String>,
    ) -> Result<(), ChainError> {
        validate_transaction_bytes(&tx_bytes)?;
        self.pending_relay.borrow_mut().push_back(PendingRelay {
            tx_bytes,
            correlation,
        });
        Ok(())
    }

    /// Drains everything queued so far, for an async driver to actually
    /// submit or relay-request.
    pub fn drain_pending_relay(&self) -> Vec<PendingRelay> {
        self.pending_relay.borrow_mut().drain(..).collect()
    }

    /// Records that `signature` has been submitted and is now awaiting
    /// confirmation — called after a `ChainClient::send_transaction`
    /// call succeeds, before treating anything as confirmed.
    pub fn track_awaiting_confirmation(
        &self,
        signature: String,
        slot_submitted: u64,
        correlation: Option<String>,
    ) {
        self.awaiting_confirmation
            .borrow_mut()
            .push(AwaitingConfirmation {
                signature,
                slot_submitted,
                correlation,
            });
    }

    /// A snapshot of everything currently awaiting confirmation, for an
    /// async driver to poll `ChainClient::get_signature_status` against.
    pub fn awaiting_confirmations(&self) -> Vec<AwaitingConfirmation> {
        self.awaiting_confirmation.borrow().clone()
    }

    /// Removes and returns the tracked entry for `signature` once its
    /// outcome (success or failure) has been observed — callable exactly
    /// once per signature; a second call for the same signature returns
    /// `None`.
    pub fn resolve_confirmation(&self, signature: &str) -> Option<AwaitingConfirmation> {
        let mut awaiting = self.awaiting_confirmation.borrow_mut();
        let index = awaiting.iter().position(|a| a.signature == signature)?;
        Some(awaiting.remove(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_no_blockhash_and_an_empty_queue() {
        let state = ChainState::new(NodeChainMode::GossipOnly);
        assert_eq!(state.current_blockhash(), None);
        assert!(state.drain_pending_relay().is_empty());
        assert!(state.awaiting_confirmations().is_empty());
    }

    #[test]
    fn records_and_reports_the_current_blockhash() {
        let state = ChainState::new(NodeChainMode::GossipOnly);
        state.record_blockhash("hash-a", 10);
        assert_eq!(state.current_blockhash(), Some(("hash-a".to_string(), 10)));
        assert!(state.current_blockhash_age().is_some());
    }

    #[test]
    fn rejects_a_malformed_transaction_before_queuing_it() {
        let state = ChainState::new(NodeChainMode::GossipOnly);
        assert_eq!(
            state.enqueue_relay(vec![1, 2, 3], None),
            Err(ChainError::MalformedTransaction)
        );
        assert!(state.drain_pending_relay().is_empty());
    }

    #[test]
    fn draining_returns_everything_queued_and_then_empties() {
        let state = ChainState::new(NodeChainMode::GossipOnly);
        // A real signed transaction's bytes aren't needed to prove
        // queue/drain semantics; validation itself is covered by
        // `validate`'s own tests plus the rejection test above.
        // `enqueue_relay` is exercised end-to-end (real transaction
        // bytes) by the RPC layer's own tests instead.
        state.pending_relay.borrow_mut().push_back(PendingRelay {
            tx_bytes: vec![9, 9, 9],
            correlation: None,
        });
        state.pending_relay.borrow_mut().push_back(PendingRelay {
            tx_bytes: vec![8, 8, 8],
            correlation: Some("set-1".to_string()),
        });
        let drained = state.drain_pending_relay();
        assert_eq!(drained[0].tx_bytes, vec![9, 9, 9]);
        assert_eq!(drained[1].correlation, Some("set-1".to_string()));
        assert!(state.drain_pending_relay().is_empty());
    }

    #[test]
    fn tracks_and_resolves_an_awaiting_confirmation_exactly_once() {
        let state = ChainState::new(NodeChainMode::GossipOnly);
        state.track_awaiting_confirmation("sig-1".to_string(), 42, Some("set-1".to_string()));
        assert_eq!(state.awaiting_confirmations().len(), 1);

        let resolved = state.resolve_confirmation("sig-1").unwrap();
        assert_eq!(resolved.slot_submitted, 42);
        assert_eq!(resolved.correlation, Some("set-1".to_string()));
        assert!(state.awaiting_confirmations().is_empty());
        assert!(state.resolve_confirmation("sig-1").is_none());
    }
}
