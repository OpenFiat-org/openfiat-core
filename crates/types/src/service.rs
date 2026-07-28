//! Service Registry types (SRP §6-8): what a service *is* and how it's identified.
//!
//! Registration, health states, and metadata schemas are `openfiat-registry`'s
//! concern (Phase 5) — this module only defines the shared vocabulary other
//! crates need to reference a service without depending on the registry
//! itself (e.g. an RPC response naming which service produced a record).

/// A globally unique, permanent identifier for a registered service (SRP §8).
///
/// Stable for the lifetime of the service: changing its endpoints or
/// metadata never changes the `ServiceId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ServiceId(String);

impl ServiceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Infrastructure-category services (SRP §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum InfrastructureService {
    BootstrapNode,
    SnapshotProvider,
    PublicApiNode,
}

/// Marketplace-category services (SRP §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MarketplaceService {
    MerchantGateway,
    AnalyticsProvider,
}

/// Notification delivery channels (SRP §6, §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum NotificationChannel {
    Email,
    Telegram,
    Sms,
    Push,
    Webhook,
}

/// Market-data services (SRP §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MarketDataService {
    PriceOracle,
    FxOracle,
}

/// Security-category services (SRP §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SecurityService {
    RiskIntelligenceProvider,
    WalletFlaggingProvider,
}

/// The full set of service types a node may register (SRP §6).
///
/// Mirrors the spec's own category grouping (Infrastructure / Marketplace /
/// Notifications / Market Data / Security) rather than flattening every
/// variant into one enum, so a match on the outer category reads the same
/// way the spec's table does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ServiceType {
    Infrastructure(InfrastructureService),
    Marketplace(MarketplaceService),
    Notifications(NotificationChannel),
    MarketData(MarketDataService),
    Security(SecurityService),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_id_equality_is_string_equality() {
        assert_eq!(ServiceId::new("svc-1"), ServiceId::new("svc-1"));
        assert_ne!(ServiceId::new("svc-1"), ServiceId::new("svc-2"));
    }
}
