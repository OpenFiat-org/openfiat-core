//! The settlement shape and its state machine (OFS-2300 §5, §20).
//!
//! Actual on-chain escrow release (§16 — "escrow release is performed
//! exclusively by the OpenFiat Program") is a Solana instruction; this
//! P2P coordination layer never constructs or signs it — that's the
//! seller's own wallet, client-side, via OFS-4300's `sendTransaction`
//! (or a peer relay if their node has no direct RPC connection). What
//! this crate *does* do is hold `Approved` as a real, distinct state
//! (the merchant has approved; release hasn't been confirmed on-chain
//! yet) and record the transition to `Completed` once something outside
//! this crate independently observes that confirmation — the same
//! "local, unsigned bookkeeping" pattern `openfiat-reservations` already
//! uses for timeout expiry, not a new signed peer-to-peer event, since
//! on-chain confirmation is equally independently verifiable by every
//! node (OFS-4300 §7-8), not something one peer asserts to another.

use openfiat_reservations::ReservationId;
use openfiat_types::{Amount, PeerId, PublicKey, Timestamp};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SettlementId(String);

impl SettlementId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §20's authoritative settlement state machine, from `EscrowLocked`
/// onward. `PaymentSubmitted` also stands in for "Merchant Reviewing" —
/// the same underlying condition (payment declared, awaiting merchant
/// decision), not a separately persisted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SettlementState {
    AwaitingPayment,
    PaymentSubmitted,
    Approved,
    Completed,
    Rejected,
    Cancelled,
    /// Escalated to Dispute (OFS-2400) — that crate takes over resolution
    /// of a settlement in this state; this crate doesn't depend on it.
    Disputed,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settlement {
    pub id: SettlementId,
    pub reservation_id: ReservationId,
    pub buyer: PeerId,
    pub buyer_public_key: PublicKey,
    pub seller: PeerId,
    pub seller_public_key: PublicKey,
    pub amount: Amount,
    pub state: SettlementState,
    pub payment_reference: Option<String>,
    /// The on-chain `release_escrow` transaction's own signature, once
    /// its confirmation has been independently observed (OFS-4300) —
    /// `None` until `SettlementRegistry::apply_escrow_released`.
    pub escrow_release_signature: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
