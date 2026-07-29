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
/// Availability (§13 — online time, response latency) and Payment
/// Accuracy (§14 — payment-detail mismatches) need signals this
/// workspace doesn't produce yet (node-level presence tracking, payment-
/// detail mismatch reporting); `reservations_missed` covers the one §13
/// signal ("missed reservations") this crate can already compute, and the
/// rest are deferred the same way `identity`'s OTP delivery and
/// `settlement`'s on-chain escrow release are.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReputationProfile {
    pub wallet: PeerId,
    pub trades_started: u64,
    pub trades_completed: u64,
    pub trades_cancelled: u64,
    pub disputes_involved: u64,
    pub disputes_lost: u64,
    pub reservations_missed: u64,
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
