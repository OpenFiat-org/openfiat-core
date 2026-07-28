//! Event origination authorization (OGP §7): "Any authenticated node MAY
//! originate events for services it legitimately provides... Node
//! implementations MUST reject unauthorized event types."
//!
//! This table only encodes the role bindings OGP §7 names explicitly.
//! Every producing crate built in later phases (advertisements,
//! governance, oracles, ...) owns the authorization rule for the event
//! types *it* defines — this isn't meant to become the one place every
//! future event type gets registered. Event types not listed here are
//! treated as authorized by default: OGP §7's restriction is scoped to
//! specific role-tied event types, not a blanket allowlist requirement,
//! so refusing to classify an unlisted type would be inventing a rule the
//! spec doesn't make.

use openfiat_types::{EventType, NodeRole};

/// Whether a node holding `roles` may originate an event of `event_type`.
pub fn is_authorized(roles: &[NodeRole], event_type: &EventType) -> bool {
    let required_role = match event_type.as_str() {
        "AdvertisementCreated" | "AdvertisementUpdated" => Some(NodeRole::MerchantGateway),
        "FXPriceUpdated" => Some(NodeRole::OracleProvider),
        "NotificationStatus" => Some(NodeRole::NotificationGateway),
        "WalletFlagged" => Some(NodeRole::RiskIntelligenceProvider),
        _ => None,
    };

    match required_role {
        Some(role) => roles.contains(&role),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merchant_gateway_may_originate_advertisement_events() {
        let event_type = EventType::new("AdvertisementCreated").unwrap();
        assert!(is_authorized(&[NodeRole::MerchantGateway], &event_type));
    }

    #[test]
    fn a_full_node_may_not_originate_advertisement_events() {
        let event_type = EventType::new("AdvertisementCreated").unwrap();
        assert!(!is_authorized(&[NodeRole::FullNode], &event_type));
    }

    #[test]
    fn an_oracle_provider_may_not_originate_wallet_flags() {
        let event_type = EventType::new("WalletFlagged").unwrap();
        assert!(!is_authorized(&[NodeRole::OracleProvider], &event_type));
    }

    #[test]
    fn unlisted_event_types_default_to_authorized() {
        let event_type = EventType::new("SomeFutureEventType").unwrap();
        assert!(is_authorized(&[], &event_type));
    }
}
