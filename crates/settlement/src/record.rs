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
    /// Escalated to Dispute (OFS-2400) — the escrow is frozen and this
    /// settlement's own transitions are suspended until arbitration
    /// concludes.
    ///
    /// Written by [`crate::SettlementRegistry::apply_dispute_opened`] and
    /// left by [`crate::SettlementRegistry::apply_dispute_resolved`], both
    /// called by `openfiat-disputes` — which already holds this registry,
    /// so the dependency still runs one way and this crate still knows
    /// nothing about disputes beyond what the escrow did.
    ///
    /// It has to be both, not either. Entry without an exit would be
    /// worse than the nothing that was here before: dispute resolution
    /// terminates on the dispute record, so a settlement parked in
    /// `Disputed` would never satisfy `apply_escrow_released`'s
    /// `Approved` precondition and every arbitrated trade would strand
    /// here permanently.
    Disputed,
}

/// What arbitration did to the escrow, in the only terms this crate has
/// any business knowing (OFS-2400 §17, OFS-4200 §2).
///
/// Not `openfiat_disputes::Resolution` re-exported: that enum names
/// *verdicts* (`BuyerWins`, `MutualSettlement`, …) and lives downstream
/// of this crate, so depending on it would be the cycle. Two outcomes are
/// all the settlement state machine can act on, because the two are the
/// only distinction the escrow makes — the program's
/// `execute_dispute_outcome` either releases the trade escrow to the
/// buyer or unwinds it back to the merchant's liquidity vault, and the
/// mapping from four verdicts onto these two lives in
/// `openfiat-disputes`, next to the verdicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DisputeVerdict {
    /// The escrow was released to the buyer — what an uncontested
    /// approval would have done, reached by arbitration instead.
    EscrowReleased,
    /// The escrow was returned to the merchant. No transfer happened, so
    /// this is an abandoned trade rather than a completed one.
    EscrowReturned,
}

/// Why a merchant rejected a submitted payment, as one of OFS-3000 §14's
/// named settlement-discrepancy kinds.
///
/// `SettlementRejected` already carries a free-text `reason` for a human
/// to read. This enum exists alongside it because OFS-3000 §14 makes
/// payment accuracy a *reputation dimension*, and a dimension has to be
/// counted, not read — parsing prose to decide whether a rejection was a
/// wrong-amount or a wrong-reference would be guesswork, and guesswork
/// that silently mis-attributes a reputation penalty is worse than no
/// signal at all.
///
/// `Other` is deliberately not a catch-all for laziness: a rejection that
/// is not about the payment's details (say, the merchant simply changed
/// their mind) must not count against the buyer's payment accuracy, and
/// [`PaymentDiscrepancy::is_payment_accuracy_fault`] is what draws that
/// line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PaymentDiscrepancy {
    /// §14: "Incorrect payment amount".
    IncorrectAmount,
    /// §14: "Wrong payment reference".
    WrongReference,
    /// §14: "Duplicate payments".
    DuplicatePayment,
    /// §14: "Incorrect account usage".
    IncorrectAccount,
    /// A rejection unrelated to the payment's details.
    Other,
}

impl PaymentDiscrepancy {
    /// Whether this rejection reflects a payment-detail mistake by the
    /// payer, and so counts toward §14. `Other` does not.
    pub fn is_payment_accuracy_fault(self) -> bool {
        !matches!(self, PaymentDiscrepancy::Other)
    }
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
    /// When the buyer declared payment sent, taken from that signed
    /// event's own timestamp. Cleared again if they withdraw the claim
    /// (§10), so it always describes the *outstanding* declaration.
    ///
    /// Paired with `merchant_responded_at`, this is what makes OFS-3000
    /// §13's response rate and response latency computable without any
    /// new event: both endpoints are already signed and replicated.
    #[serde(default)]
    pub payment_submitted_at: Option<Timestamp>,
    /// When the merchant approved or rejected that declaration, from
    /// their own signed event's timestamp.
    #[serde(default)]
    pub merchant_responded_at: Option<Timestamp>,
    /// Set only on rejection — §14's typed discrepancy kind.
    #[serde(default)]
    pub payment_discrepancy: Option<PaymentDiscrepancy>,
    /// When this settlement was escalated to arbitration (OFS-2400), from
    /// the opener's own signed event timestamp. `None` if it never was.
    ///
    /// Kept after the dispute resolves, when `state` has moved on to
    /// `Completed` or `Cancelled` and no longer says anything happened.
    /// "Was this trade arbitrated?" is a question about the trade's
    /// history, and this is the only place the answer survives — it is
    /// what lets `openfiat-trade` answer it without holding the dispute
    /// registry, and by construction it can only ever be read by someone
    /// already reading the settlement.
    #[serde(default)]
    pub disputed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
