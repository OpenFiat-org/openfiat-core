//! `openfiat-oracles` — plugin architecture and provider SDK for OpenFiat
//! data oracles (exchange rates, stablecoin metadata, payment networks,
//! country metadata, and future providers such as weather).
//!
//! Related specification: OFS-7000 (OpenFiat Oracle Protocol).
//!
//! This crate defines the `OracleProvider` interface only. No concrete
//! providers are implemented here — each is expected to live in its own
//! crate/plugin implementing this trait.

/// Crate version, re-exported for diagnostics and `openfiat-node --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A single exchange-rate observation.
#[derive(Debug, Clone, PartialEq)]
pub struct ExchangeRate {
    pub base: String,
    pub quote: String,
    pub rate: f64,
    pub as_of_unix: i64,
}

/// Implemented by an oracle data provider plugin.
///
/// No implementations ship in this crate — providers (exchange rates,
/// stablecoin metadata, payment networks, country metadata, and future
/// categories such as weather) are supplied externally.
pub trait OracleProvider: Send + Sync {
    /// Stable identifier for this provider, e.g. `"exchange-rates.example"`.
    fn name(&self) -> &str;

    /// Fetch the current exchange rate between two currency/asset codes.
    fn fetch_exchange_rate(&self, base: &str, quote: &str) -> Result<ExchangeRate, OracleError>;
}

/// Errors an [`OracleProvider`] may return.
#[derive(Debug)]
pub enum OracleError {
    NotImplemented,
    Unavailable(String),
}

/// In-memory registry of configured oracle providers.
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
    fn reports_a_version() {
        assert!(!version().is_empty());
    }

    #[test]
    fn registry_starts_empty() {
        let registry = OracleRegistry::new();
        assert!(registry.is_empty());
    }
}
