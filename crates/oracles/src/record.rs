//! The oracle record shape (OFS-7000 §6-7, §12).

use openfiat_types::{PeerId, PublicKey, Timestamp};

/// §6's initial category list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OracleCategory {
    ExchangeRate,
    StablecoinMetadata,
    PaymentInfrastructure,
    RegionalMetadata,
}

/// §6/§9-10's field lists, one variant per category — a record only ever
/// carries the fields relevant to its own category, not a single
/// catch-all shape every category has to squeeze into.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum OracleData {
    /// §9: e.g. "1 USDC ≈ 129.52 KES".
    ExchangeRate { base: String, quote: String, rate: f64 },
    /// §10.
    StablecoinMetadata { symbol: String, name: String, decimals: u8, blockchain: String, mint_address: Option<String>, website: Option<String>, status: String },
    /// §6's Payment Infrastructure examples.
    PaymentInfrastructure { rail: String, available: bool, note: Option<String> },
    /// §6's Regional Metadata examples.
    RegionalMetadata { country: String, supported_fiat: Vec<String>, payment_methods: Vec<String> },
}

impl OracleData {
    pub const fn category(&self) -> OracleCategory {
        match self {
            Self::ExchangeRate { .. } => OracleCategory::ExchangeRate,
            Self::StablecoinMetadata { .. } => OracleCategory::StablecoinMetadata,
            Self::PaymentInfrastructure { .. } => OracleCategory::PaymentInfrastructure,
            Self::RegionalMetadata { .. } => OracleCategory::RegionalMetadata,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OracleId(String);

impl OracleId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// §7's field list.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OracleRecord {
    pub id: OracleId,
    pub provider: PeerId,
    pub provider_public_key: PublicKey,
    pub data: OracleData,
    pub version: u64,
    pub published_at: Timestamp,
    pub expires_at: Timestamp,
}

impl OracleRecord {
    /// §12: "Expired data SHOULD NOT be treated as current."
    pub fn is_current(&self, now: Timestamp) -> bool {
        now.as_millis() < self.expires_at.as_millis()
    }
}
