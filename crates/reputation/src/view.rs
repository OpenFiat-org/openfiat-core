//! §29: the Reputation Engine integrates information from nearly every
//! other trading protocol rather than owning a protocol of its own.
//!
//! Reputation deliberately has no signed event type or store of its own
//! (contrast with every other crate in this workspace): §26 requires
//! "only cryptographically verified protocol events SHALL modify
//! reputation", and a wallet-signed "I completed a trade" claim about
//! itself is exactly the kind of self-asserted signal §20's
//! anti-manipulation goals rule out. Instead, like `openfiat-trade`, this
//! is a pure read-side aggregate computed directly from Reservations',
//! Settlement's, and Disputes' already gossip-replicated state — since
//! every node already converges on identical state there (each crate's
//! own replication), recomputing reputation from it deterministically
//! gives every node an identical reputation view for free, satisfying
//! §24 without a second gossip channel or a second place abuse could
//! target.

use crate::record::ReputationProfile;
use openfiat_disputes::{DisputeRegistry, Resolution};
use openfiat_reservations::{ReservationRegistry, ReservationState};
use openfiat_settlement::{SettlementRegistry, SettlementState};
use openfiat_storage::KvStore;
use openfiat_types::PeerId;
use std::rc::Rc;

/// Reads a consistent reputation view from the shared reservation,
/// settlement, and dispute registries a node already maintains — see
/// `ReservationService::registry`/`SettlementService::registry`/
/// `DisputeService::registry`.
pub struct ReputationView<S> {
    reservations: Rc<ReservationRegistry<S>>,
    settlements: Rc<SettlementRegistry<S>>,
    disputes: Rc<DisputeRegistry<S>>,
}

impl<S: KvStore> ReputationView<S> {
    pub fn new(
        reservations: Rc<ReservationRegistry<S>>,
        settlements: Rc<SettlementRegistry<S>>,
        disputes: Rc<DisputeRegistry<S>>,
    ) -> Self {
        Self {
            reservations,
            settlements,
            disputes,
        }
    }

    /// Computed on demand — O(reservations + settlements + disputes) per
    /// call. Fine at the scale a single node's local replica holds; if
    /// that stops being true, maintain running per-wallet aggregates
    /// incrementally instead of rescanning.
    pub fn profile(&self, wallet: &PeerId) -> ReputationProfile {
        let mut profile = ReputationProfile::empty(wallet.clone());

        for settlement in self.settlements.all() {
            if &settlement.buyer != wallet && &settlement.seller != wallet {
                continue;
            }
            profile.trades_started += 1;
            profile.observe_activity_at(settlement.created_at);

            // §13/§14 are attributed to opposite sides of the same trade:
            // the merchant is judged on answering a payment declaration,
            // the payer on getting the payment's details right.
            if &settlement.seller == wallet
                && let Some(submitted) = settlement.payment_submitted_at
            {
                match settlement.merchant_responded_at {
                    Some(responded) => profile.record_payment_response(
                        responded.as_millis().saturating_sub(submitted.as_millis()),
                    ),
                    None => profile.record_payment_response_outstanding(),
                }
            }
            if &settlement.buyer == wallet {
                // A withdrawn declaration (§10) clears `payment_submitted_at`,
                // so it neither counts as a payment made nor can be faulted.
                if settlement.payment_submitted_at.is_some() {
                    profile.payments_submitted += 1;
                }
                if settlement
                    .payment_discrepancy
                    .is_some_and(|kind| kind.is_payment_accuracy_fault())
                {
                    profile.payment_discrepancies += 1;
                }
            }
            match settlement.state {
                // Counted together, and `Approved` is the one that
                // matters: it is the last signed peer-to-peer act in a
                // trade, where `Completed` is each node's own observation
                // of the chain (`apply_escrow_released`). Counting only
                // `Completed` would make a wallet's reputation depend on
                // which node you asked.
                SettlementState::Approved | SettlementState::Completed => {
                    profile.trades_completed += 1;
                    profile.record_volume(settlement.amount);
                    profile.record_completed_duration(
                        settlement
                            .updated_at
                            .as_millis()
                            .saturating_sub(settlement.created_at.as_millis()),
                    );
                }
                SettlementState::Cancelled | SettlementState::Rejected => {
                    profile.trades_cancelled += 1
                }
                // Nothing is counted for a trade that has not finished,
                // and `Disputed` is one of those: the escrow is frozen
                // and neither party has been shown to be right yet.
                // Counting it as a cancellation would fault whichever of
                // them turns out to have been telling the truth, and
                // arbitration then moves the settlement into `Completed`
                // or `Cancelled` (OFS-2300 §5a), where it is counted once
                // — by the arm above or the one before it — as whatever
                // the chain decided it was. The dispute loop below is
                // where being *in* a dispute is counted.
                SettlementState::AwaitingPayment
                | SettlementState::PaymentSubmitted
                | SettlementState::Disputed => {}
            }
        }

        for dispute in self.disputes.all() {
            let is_buyer = &dispute.buyer == wallet;
            let is_seller = &dispute.seller == wallet;
            if !is_buyer && !is_seller {
                continue;
            }
            profile.disputes_involved += 1;
            match dispute.resolution {
                Some(Resolution::BuyerWins) if is_seller => profile.disputes_lost += 1,
                Some(Resolution::MerchantWins) if is_buyer => profile.disputes_lost += 1,
                _ => {}
            }
        }

        for reservation in self.reservations.all() {
            if &reservation.requester == wallet && reservation.state == ReservationState::Expired {
                profile.reservations_missed += 1;
            }
        }

        profile
    }
}
