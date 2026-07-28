//! The advertisement shape (OFS-2100 §4-6) and its materialized state.

use openfiat_types::{Amount, PeerId, PublicKey, Timestamp};

/// A globally unique, permanent identifier for an advertisement (§5).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct AdvertisementId(String);

impl AdvertisementId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which side of the trade the merchant is offering (§4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Direction {
    /// Merchant offers stablecoins, backed by their Liquidity Vault (§9).
    Sell,
    /// Merchant wishes to purchase stablecoins.
    Buy,
}

/// §11. Floating pricing's actual price resolution (oracle mid-price +
/// premium) is Oracle Provider integration (OFS-7000, a later phase) —
/// this only carries the configuration, not a live price.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PricingModel {
    Fixed {
        price: Amount,
    },
    Floating {
        oracle_provider: String,
        premium_bps: i32,
    },
}

/// §18: automatic-disable / vacation-mode / deletion states. Merchant
/// session presence (Online/Busy/Away, §14-15) is a UI/notification-layer
/// concern, not modeled here — this is only the advertisement's own
/// visibility state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AdvertisementStatus {
    Active,
    /// §18: liquidity hit zero, permissions lost, or manually disabled.
    Disabled,
    /// §16: merchant-initiated pause; distinct from `Disabled` so a client
    /// can tell "temporarily paused" from "something's wrong".
    Vacation,
    /// §21: permanently removed.
    Deleted,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Advertisement {
    pub id: AdvertisementId,
    pub merchant: PeerId,
    pub merchant_public_key: PublicKey,
    pub asset: String,
    pub direction: Direction,
    pub fiat_currency: String,
    pub min_trade: Amount,
    pub max_trade: Amount,
    /// §9: for a Sell ad, the unreserved balance in the merchant's
    /// Liquidity Vault. For a Buy ad, remaining declared purchasing
    /// capacity. Adjusted automatically by reservation/settlement events
    /// once those crates exist (§10) — never edited by a fresh merchant
    /// signature per trade.
    pub available_liquidity: Amount,
    pub pricing: PricingModel,
    pub payment_methods: Vec<String>,
    pub status: AdvertisementStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
