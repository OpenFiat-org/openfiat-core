//! Reputation dimensions and profile shape (OFS-3000 §6-14, §18).

use openfiat_types::{Amount, PeerId, Timestamp};

/// §18's example tier ladder. Real tier thresholds are governance-defined
/// (§18: "Tier requirements are defined by governance") — the thresholds
/// [`ReputationProfile::tier`] uses today are placeholder defaults
/// `[PROPOSED — NEEDS SIGN-OFF]`, the same pattern this workspace uses for
/// every other protocol parameter the specs leave to implementations.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum MerchantTier {
    Explorer,
    Verified,
    Professional,
    Elite,
    Institutional,
}

/// A wallet's reputation, aggregated from Settlement, Dispute, and
/// Reservation state (§5: "Every wallet possesses its own reputation
/// profile... tied to the wallet, not to a device or application").
///
/// ## Availability (§13) and Payment Accuracy (§14)
///
/// Both dimensions are computed here from the timestamps and typed
/// rejection reasons that settlement's already-signed events carry —
/// no new event type, no new gossip channel, and nothing self-asserted
/// (§20, §26). Of §13's five listed signals:
///
/// - *Response rate* and *response latency* — computed, as the gap
///   between the buyer's signed "I paid" and the merchant's signed
///   approve/reject.
/// - *Missed reservations* — computed, as `reservations_missed`.
/// - *Reservation acceptance* — **not a signal in this protocol.** A
///   reservation against an active advertisement whose amount is within
///   the published limits is accepted automatically
///   (`ReservationRegistry::apply_request`); there is no merchant
///   accept/decline step to measure, so an acceptance rate would be a
///   constant 1.0 dressed up as information.
/// - *Online time* — **not computable.** Merchant session presence is
///   deliberately not protocol state (see `openfiat-advertisements`'
///   `AdvertisementStatus`: "Merchant session presence (Online/Busy/Away)
///   is a UI/notification-layer concern, not modeled here"), and the only
///   way to introduce it would be a wallet asserting its own uptime —
///   precisely the self-reported signal §20 and §26 exclude. It stays
///   deferred until presence is attested by something a third party can
///   verify.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReputationProfile {
    pub wallet: PeerId,
    pub trades_started: u64,
    pub trades_completed: u64,
    pub trades_cancelled: u64,
    pub disputes_involved: u64,
    pub disputes_lost: u64,
    pub reservations_missed: u64,
    /// §13: payment declarations this wallet, as merchant, was asked to
    /// rule on. The denominator of the response rate.
    pub payment_responses_due: u64,
    /// §13: how many of those it actually approved or rejected.
    pub payment_responses_made: u64,
    /// §14: payment declarations this wallet made as payer. The
    /// denominator of the discrepancy rate.
    pub payments_submitted: u64,
    /// §14: rejections attributed to this wallet as payer, counting only
    /// genuine payment-detail faults — see
    /// `PaymentDiscrepancy::is_payment_accuracy_fault`.
    pub payment_discrepancies: u64,
    response_latency_sum_ms: u64,
    /// Lifetime volume (§10), bucketed by decimal precision as a coarse
    /// proxy for asset — `Amount` has no currency/mint identifier yet, so
    /// summing across genuinely different assets that happen to share a
    /// decimal count isn't distinguished here.
    pub total_volume: Vec<Amount>,
    completed_duration_sum_ms: u64,
    pub first_active_at: Option<Timestamp>,
}

impl ReputationProfile {
    pub(crate) fn empty(wallet: PeerId) -> Self {
        Self {
            wallet,
            trades_started: 0,
            trades_completed: 0,
            trades_cancelled: 0,
            disputes_involved: 0,
            disputes_lost: 0,
            reservations_missed: 0,
            payment_responses_due: 0,
            payment_responses_made: 0,
            payments_submitted: 0,
            payment_discrepancies: 0,
            response_latency_sum_ms: 0,
            total_volume: Vec::new(),
            completed_duration_sum_ms: 0,
            first_active_at: None,
        }
    }

    /// §8: completed trades ÷ started trades.
    pub fn trade_success_rate(&self) -> Option<f64> {
        (self.trades_started > 0).then(|| self.trades_completed as f64 / self.trades_started as f64)
    }

    /// §9: disputed trades ÷ completed trades.
    pub fn dispute_rate(&self) -> Option<f64> {
        (self.trades_completed > 0)
            .then(|| self.disputes_involved as f64 / self.trades_completed as f64)
    }

    /// §7: mean time from settlement start to completion, in milliseconds.
    pub fn average_settlement_duration_ms(&self) -> Option<f64> {
        (self.trades_completed > 0)
            .then(|| self.completed_duration_sum_ms as f64 / self.trades_completed as f64)
    }

    /// §11: mean completed-trade size, per volume bucket.
    pub fn average_ticket_size(&self) -> Vec<(Amount, f64)> {
        if self.trades_completed == 0 {
            return Vec::new();
        }
        self.total_volume
            .iter()
            .map(|total| {
                (
                    *total,
                    total.base_units() as f64 / self.trades_completed as f64,
                )
            })
            .collect()
    }

    /// §13: how often this wallet, as merchant, answered a payment
    /// declaration at all. `None` when none has been put to it.
    pub fn response_rate(&self) -> Option<f64> {
        (self.payment_responses_due > 0)
            .then(|| self.payment_responses_made as f64 / self.payment_responses_due as f64)
    }

    /// §13: mean time in milliseconds from a buyer declaring payment to
    /// this wallet approving or rejecting it. Averaged over responses
    /// actually made — an unanswered declaration has no latency to
    /// average, and is already counted against [`Self::response_rate`].
    pub fn average_response_latency_ms(&self) -> Option<f64> {
        (self.payment_responses_made > 0)
            .then(|| self.response_latency_sum_ms as f64 / self.payment_responses_made as f64)
    }

    /// §14: share of this wallet's payment declarations that a merchant
    /// rejected over the payment's own details. `None` until it has paid
    /// at least once.
    pub fn payment_discrepancy_rate(&self) -> Option<f64> {
        (self.payments_submitted > 0)
            .then(|| self.payment_discrepancies as f64 / self.payments_submitted as f64)
    }

    /// §12: time since this wallet's first observed trade.
    pub fn merchant_age_ms(&self, now: Timestamp) -> Option<u64> {
        self.first_active_at
            .map(|first| now.as_millis().saturating_sub(first.as_millis()))
    }

    /// §18 — see the type-level doc: thresholds are a placeholder pending
    /// real governance parameters.
    pub fn tier(&self) -> MerchantTier {
        match self.trades_completed {
            1000.. => MerchantTier::Institutional,
            250..1000 => MerchantTier::Elite,
            50..250 => MerchantTier::Professional,
            5..50 => MerchantTier::Verified,
            _ => MerchantTier::Explorer,
        }
    }

    pub(crate) fn record_volume(&mut self, amount: Amount) {
        match self
            .total_volume
            .iter_mut()
            .find(|existing| existing.decimals() == amount.decimals())
        {
            Some(existing) => {
                if let Some(sum) = existing.checked_add(amount) {
                    *existing = sum;
                }
            }
            None => self.total_volume.push(amount),
        }
    }

    /// A payment declaration was put to this wallet as merchant, and
    /// answered `latency_ms` later.
    pub(crate) fn record_payment_response(&mut self, latency_ms: u64) {
        self.payment_responses_due += 1;
        self.payment_responses_made += 1;
        self.response_latency_sum_ms += latency_ms;
    }

    /// A payment declaration was put to this wallet as merchant and is
    /// still unanswered — counted against the response rate, with no
    /// latency to record.
    pub(crate) fn record_payment_response_outstanding(&mut self) {
        self.payment_responses_due += 1;
    }

    pub(crate) fn record_completed_duration(&mut self, duration_ms: u64) {
        self.completed_duration_sum_ms += duration_ms;
    }

    pub(crate) fn observe_activity_at(&mut self, at: Timestamp) {
        self.first_active_at = Some(match self.first_active_at {
            Some(existing) if existing.as_millis() <= at.as_millis() => existing,
            _ => at,
        });
    }
}
