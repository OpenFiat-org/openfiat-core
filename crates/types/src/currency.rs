//! A fiat currency code, in one spelling.
//!
//! # What this checks, and what it deliberately does not
//!
//! It checks the *form*: three ASCII letters, as ISO 4217 defines a
//! currency code. It does not check membership of any list.
//!
//! That restraint is the same one `openfiat_crypto::MintAddress` makes,
//! and it is the position this crate already took elsewhere:
//! `PricingModel::Floating` carries a merchant-declared `price_decimals`
//! rather than inferring precision from the currency, on the stated
//! grounds that a hardcoded currency table in a protocol crate "silently
//! mis-rounds every currency missing from it". A membership check would
//! have the same defect one step earlier — a node built before a currency
//! was added would reject a perfectly good advertisement, and two honest
//! nodes on different releases would disagree about which advertisements
//! are valid.
//!
//! # Why a newtype at all, if it only checks three letters
//!
//! Because `fiat_currency` was a bare `String` that nothing validated, so
//! `KES`, `kes`, `Kenyan Shillings` and `""` were all equally acceptable
//! on a signed, replicated record. Three consequences, none hypothetical:
//! a filter had to compare case-insensitively to work at all, an order
//! book could show the same corridor under several headings, and an
//! interface had no way to know whether a value was a currency code or a
//! sentence somebody typed.
//!
//! Normalising to uppercase at the door makes `KES == KES` mean what it
//! looks like it means, which is the property every consumer here was
//! quietly assuming.

/// The only way a currency code can be wrong here: it is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrencyError {
    Malformed,
}

impl std::fmt::Display for CurrencyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not a three-letter ISO 4217 currency code")
    }
}

impl std::error::Error for CurrencyError {}

/// An ISO 4217 alphabetic currency code, uppercase.
///
/// Construct with [`FiatCurrency::parse`]. No `From<String>`, no public
/// field, and a `Deserialize` that goes through the parser — a value
/// arriving in a gossiped record has crossed a trust boundary exactly
/// like one typed into a form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct FiatCurrency(String);

impl FiatCurrency {
    pub fn parse(input: &str) -> Result<Self, CurrencyError> {
        // Trimmed before checking, because a trailing space is a typing
        // slip rather than a different currency, and refusing it would
        // fail a merchant for something no consumer would have noticed.
        let trimmed = input.trim();
        if trimmed.len() != 3 || !trimmed.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(CurrencyError::Malformed);
        }
        Ok(Self(trimmed.to_ascii_uppercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FiatCurrency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for FiatCurrency {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        FiatCurrency::parse(&raw)
            .map_err(|_| serde::de::Error::custom("not a three-letter ISO 4217 currency code"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_currency_has_one_spelling() {
        // The property every consumer was already assuming. Before this,
        // a filter had to compare case-insensitively to work at all.
        for spelling in ["KES", "kes", "Kes", " kes "] {
            assert_eq!(FiatCurrency::parse(spelling).unwrap().as_str(), "KES");
        }
        assert_eq!(
            FiatCurrency::parse("kes").unwrap(),
            FiatCurrency::parse("KES").unwrap()
        );
    }

    #[test]
    fn a_sentence_is_not_a_currency_code() {
        for hostile in ["", "K", "KESH", "Kenyan Shillings", "K3S", "€", "US$"] {
            assert_eq!(
                FiatCurrency::parse(hostile),
                Err(CurrencyError::Malformed),
                "{hostile:?} must not reach a signed record"
            );
        }
    }

    #[test]
    fn a_currency_this_build_never_heard_of_is_still_accepted() {
        // The whole reason there is no membership table. A node built
        // last year must not reject an advertisement in a currency added
        // since, or two honest nodes on different releases disagree about
        // which advertisements are valid.
        for real_but_obscure in ["MRU", "STN", "VES", "ZWG"] {
            assert!(FiatCurrency::parse(real_but_obscure).is_ok());
        }
        // And something that is not a currency at all, but is shaped like
        // one, is accepted too. That is the honest cost of checking form
        // rather than membership, and it is cheaper than the alternative.
        assert!(FiatCurrency::parse("XYZ").is_ok());
    }

    #[test]
    fn deserialization_cannot_mint_an_unvalidated_code() {
        let valid: FiatCurrency = serde_json::from_str("\"kes\"").expect("a real code round-trips");
        assert_eq!(valid.as_str(), "KES");

        let sentence: Result<FiatCurrency, _> = serde_json::from_str("\"Kenyan Shillings\"");
        assert!(
            sentence.is_err(),
            "a peer's gossip must not introduce a currency that never passed the parser"
        );
    }
}
