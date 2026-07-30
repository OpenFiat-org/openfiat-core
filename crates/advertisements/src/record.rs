//! The advertisement shape (OFS-2100 §4-6) and its materialized state.

use openfiat_crypto::MintAddress;
use openfiat_types::{Amount, FiatCurrency, PeerId, PublicKey, Timestamp};

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

/// §11. How an advertisement's fiat-per-asset price is arrived at.
///
/// Both variants carry only configuration — no resolved number is stored
/// on the advertisement itself, including for `Floating`. Writing a
/// periodically-refreshed price back onto the record would make it both
/// stale (between refreshes) and divergent (each node refreshing on its
/// own clock, from its own oracle view, into a record that replicates by
/// gossip). See [`crate::pricing`] for where the number is produced
/// instead, and when.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PricingModel {
    Fixed {
        price: Amount,
    },
    Floating {
        oracle_provider: String,
        /// Basis points over the oracle mid-price. Signed: a merchant may
        /// legitimately quote *below* mid (a Buy ad competing for flow),
        /// so this is not a `u32`. `-10_000` is exactly zero; anything
        /// below that is a negative price and resolves as unpriceable
        /// rather than wrapping.
        premium_bps: i32,
        /// The decimal precision the resolved price is quoted in — the
        /// fiat currency's, e.g. 2 for KES/NGN/USD, 0 for JPY.
        ///
        /// Declared by the merchant because nothing else on the record
        /// carries it: `min_trade`, `max_trade` and `available_liquidity`
        /// are all denominated in the *asset*, and a `Floating` ad has no
        /// `Fixed` price to borrow the precision from. Inferring it from
        /// `fiat_currency` would mean a hardcoded currency table in the
        /// protocol crate that silently mis-rounds every currency missing
        /// from it.
        price_decimals: u8,
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
    /// The token a buyer receives, named by its mint address.
    ///
    /// This was `asset: String` — free text the merchant chose, shown to
    /// the buyer as what they were about to be paid in, and connected to
    /// the escrowed token by nothing at all. A merchant could advertise
    /// "USDC" and settle in something else, and every layer would agree
    /// the trade completed correctly, because each did exactly what it
    /// was asked.
    ///
    /// A mint address is identity; a ticker is a label. `ServicePricing.
    /// token_mint` in the service registry reached the same conclusion
    /// for provider fees. The name a buyer sees is resolved from this at
    /// the edge — `openfiat_chain::symbol_for_mint` — so the merchant
    /// never supplies it, and a mint nobody has a name for is displayed
    /// as its address rather than as a guess.
    pub asset_mint: MintAddress,
    pub direction: Direction,
    /// The fiat side of the pair, as an ISO 4217 code.
    ///
    /// Was a bare `String` that nothing validated, so `KES`, `kes`,
    /// `Kenyan Shillings` and `""` were all equally acceptable on a
    /// signed, replicated record — which meant an order book could show
    /// one corridor under several headings and a filter had to compare
    /// case-insensitively to work at all. `FiatCurrency` normalises at
    /// the door, so equality means what it looks like it means.
    ///
    /// Checked for *form*, never for membership of a list. See
    /// `openfiat_types::currency` — and `PricingModel::Floating`'s
    /// `price_decimals` above, which reached the same conclusion first.
    pub fiat_currency: FiatCurrency,
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
