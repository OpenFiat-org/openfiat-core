//! What a notification actually says.
//!
//! OFS-6000 §19 limits a provider to "only what delivery requires", and
//! the gateway is a third party that reads whatever the node sends it.
//! So the rendered text is deliberately *contentless*: it names the event
//! class and tells the recipient to open their client. No counterparty,
//! no amount, no settlement or dispute identifier — none of that survives
//! the hop to a gateway operator who has no business holding it, and none
//! of it is needed for the message to do its job.
//!
//! Localisation is out of scope for this crate (§2 leaves the payload
//! schema to implementations, and nothing in the protocol carries a
//! locale); a gateway that wants localised copy has the `trigger` and can
//! render its own.

use crate::record::NotificationTrigger;

/// The subject and body for `trigger`.
pub fn compose(trigger: NotificationTrigger) -> (String, String) {
    let (subject, body) = match trigger {
        NotificationTrigger::ReservationCreated => (
            "Reservation created",
            "A reservation involving your wallet has been created.",
        ),
        NotificationTrigger::ReservationExpiring => (
            "Reservation expiring",
            "A reservation involving your wallet is about to expire.",
        ),
        NotificationTrigger::PaymentSubmitted => (
            "Payment submitted",
            "A buyer has declared payment sent on one of your settlements.",
        ),
        NotificationTrigger::SettlementApproved => (
            "Settlement approved",
            "A settlement involving your wallet has been approved.",
        ),
        NotificationTrigger::EscrowReleased => (
            "Escrow released",
            "Escrow has been released on a settlement involving your wallet.",
        ),
        NotificationTrigger::TradeCompleted => (
            "Trade completed",
            "A trade involving your wallet has completed.",
        ),
        NotificationTrigger::AdvertisementDisabled => (
            "Advertisement disabled",
            "One of your advertisements has been disabled.",
        ),
        NotificationTrigger::ReputationUpdated => (
            "Reputation updated",
            "Your marketplace reputation has changed.",
        ),
        NotificationTrigger::EvidenceRequested => (
            "Evidence requested",
            "A dispute involving your wallet needs your evidence.",
        ),
        NotificationTrigger::ResolutionIssued => (
            "Dispute resolved",
            "A dispute involving your wallet has been resolved.",
        ),
        NotificationTrigger::ProposalPublished => (
            "Governance proposal published",
            "A new governance proposal is open for review.",
        ),
        NotificationTrigger::VotingStarted => (
            "Voting started",
            "Voting has opened on a governance proposal.",
        ),
        NotificationTrigger::ProposalActivated => (
            "Governance proposal activated",
            "A governance proposal has been activated.",
        ),
        NotificationTrigger::SnapshotAvailable => {
            ("Snapshot available", "A new network snapshot is available.")
        }
        NotificationTrigger::NodeMaintenance => (
            "Node maintenance",
            "A node you rely on has entered maintenance.",
        ),
        NotificationTrigger::ProviderOffline => (
            "Provider offline",
            "A service provider you rely on has gone offline.",
        ),
    };
    (
        subject.to_string(),
        format!("{body} Open your OpenFiat client for the details."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_trigger_renders_non_empty_copy() {
        for trigger in [
            NotificationTrigger::ReservationCreated,
            NotificationTrigger::ReservationExpiring,
            NotificationTrigger::PaymentSubmitted,
            NotificationTrigger::SettlementApproved,
            NotificationTrigger::EscrowReleased,
            NotificationTrigger::TradeCompleted,
            NotificationTrigger::AdvertisementDisabled,
            NotificationTrigger::ReputationUpdated,
            NotificationTrigger::EvidenceRequested,
            NotificationTrigger::ResolutionIssued,
            NotificationTrigger::ProposalPublished,
            NotificationTrigger::VotingStarted,
            NotificationTrigger::ProposalActivated,
            NotificationTrigger::SnapshotAvailable,
            NotificationTrigger::NodeMaintenance,
            NotificationTrigger::ProviderOffline,
        ] {
            let (subject, body) = compose(trigger);
            assert!(!subject.is_empty(), "{trigger:?} has no subject");
            assert!(!body.is_empty(), "{trigger:?} has no body");
        }
    }

    /// §19 in test form: the gateway operator learns *that* something
    /// happened, never what or with whom.
    #[test]
    fn rendered_copy_never_carries_trade_detail() {
        let (subject, body) = compose(NotificationTrigger::SettlementApproved);
        for text in [&subject, &body] {
            assert!(!text.contains('$'));
            assert!(!text.chars().any(|c| c.is_ascii_digit()));
        }
    }
}
