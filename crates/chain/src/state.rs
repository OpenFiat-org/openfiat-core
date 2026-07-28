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

pub struct ChainState {
    mode: NodeChainMode,
    cache: RefCell<BlockhashCache>,
    pending_relay: RefCell<VecDeque<Vec<u8>>>,
}

impl ChainState {
    pub fn new(mode: NodeChainMode) -> Self {
        Self {
            mode,
            cache: RefCell::new(BlockhashCache::new()),
            pending_relay: RefCell::new(VecDeque::new()),
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
    pub fn enqueue_relay(&self, tx_bytes: Vec<u8>) -> Result<(), ChainError> {
        validate_transaction_bytes(&tx_bytes)?;
        self.pending_relay.borrow_mut().push_back(tx_bytes);
        Ok(())
    }

    /// Drains everything queued so far, for an async driver to actually
    /// submit or relay-request.
    pub fn drain_pending_relay(&self) -> Vec<Vec<u8>> {
        self.pending_relay.borrow_mut().drain(..).collect()
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
            state.enqueue_relay(vec![1, 2, 3]),
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
        state.pending_relay.borrow_mut().push_back(vec![9, 9, 9]);
        state.pending_relay.borrow_mut().push_back(vec![8, 8, 8]);
        assert_eq!(
            state.drain_pending_relay(),
            vec![vec![9, 9, 9], vec![8, 8, 8]]
        );
        assert!(state.drain_pending_relay().is_empty());
    }
}
