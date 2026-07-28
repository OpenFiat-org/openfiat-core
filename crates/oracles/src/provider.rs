//! The local plugin interface for fetching oracle data from an external
//! source (e.g. a real exchange-rate API) before publishing it as a
//! signed `OraclePublish` — kept from this crate's original scaffolding.
//! No concrete providers ship here; each is expected to live in its own
//! crate/plugin implementing this trait.

/// A single exchange-rate observation, as fetched from an external source
/// (distinct from `record::OracleRecord`, which is the signed, published,
/// replicated form of one).
#[derive(Debug, Clone, PartialEq)]
pub struct ExchangeRate {
    pub base: String,
    pub quote: String,
    pub rate: f64,
    pub as_of_unix: i64,
}

/// Errors an [`OracleProvider`] may return when fetching from its
/// external source — distinct from `error::OracleError`, which covers
/// this crate's own gossip/replication layer.
#[derive(Debug)]
pub enum ProviderError {
    NotImplemented,
    Unavailable(String),
}

/// Implemented by an oracle data provider plugin.
pub trait OracleProvider: Send + Sync {
    /// Stable identifier for this provider, e.g. `"exchange-rates.example"`.
    fn name(&self) -> &str;

    /// Fetch the current exchange rate between two currency/asset codes.
    fn fetch_exchange_rate(&self, base: &str, quote: &str) -> Result<ExchangeRate, ProviderError>;
}

/// In-memory registry of configured oracle provider plugins.
#[derive(Default)]
pub struct OracleRegistry {
    providers: Vec<Box<dyn OracleProvider>>,
}

impl OracleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, provider: Box<dyn OracleProvider>) {
        self.providers.push(provider);
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_starts_empty() {
        let registry = OracleRegistry::new();
        assert!(registry.is_empty());
    }
}
