//! Fixed-point amounts (crypto balances, fiat totals, prices).
//!
//! Money is never a float in this codebase: `Amount` stores an integer
//! count of the asset's smallest indivisible unit (lamports, cents, ...)
//! alongside the decimal precision needed to render it, and every
//! arithmetic operation is checked so overflow surfaces as `None` instead
//! of silently wrapping.

use std::fmt;

/// An amount of some asset, as an integer count of its smallest unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Amount {
    base_units: u64,
    decimals: u8,
}

impl Amount {
    /// Construct from a raw base-unit count and the asset's decimal precision.
    pub const fn new(base_units: u64, decimals: u8) -> Self {
        Self {
            base_units,
            decimals,
        }
    }

    /// The raw base-unit count.
    pub const fn base_units(self) -> u64 {
        self.base_units
    }

    /// The asset's decimal precision.
    pub const fn decimals(self) -> u8 {
        self.decimals
    }

    /// `self + other`, or `None` on overflow or a decimals mismatch.
    ///
    /// Adding amounts denominated in different precisions (e.g. lamports
    /// vs. cents) is almost always a bug at the call site, so it's rejected
    /// rather than silently rescaled.
    pub fn checked_add(self, other: Amount) -> Option<Amount> {
        (self.decimals == other.decimals)
            .then(|| self.base_units.checked_add(other.base_units))
            .flatten()
            .map(|base_units| Amount::new(base_units, self.decimals))
    }

    /// `self - other`, or `None` on underflow or a decimals mismatch.
    pub fn checked_sub(self, other: Amount) -> Option<Amount> {
        (self.decimals == other.decimals)
            .then(|| self.base_units.checked_sub(other.base_units))
            .flatten()
            .map(|base_units| Amount::new(base_units, self.decimals))
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.decimals == 0 {
            return write!(f, "{}", self.base_units);
        }
        let divisor = 10u64.pow(self.decimals as u32);
        write!(
            f,
            "{}.{:0width$}",
            self.base_units / divisor,
            self.base_units % divisor,
            width = self.decimals as usize
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_with_the_configured_decimal_point() {
        assert_eq!(Amount::new(123_456_789, 6).to_string(), "123.456789");
        assert_eq!(Amount::new(5, 0).to_string(), "5");
    }

    #[test]
    fn checked_add_rejects_mismatched_precision() {
        let usdc = Amount::new(1_000_000, 6);
        let sol = Amount::new(1_000_000_000, 9);
        assert_eq!(usdc.checked_add(sol), None);
        assert_eq!(
            usdc.checked_add(Amount::new(1, 6)),
            Some(Amount::new(1_000_001, 6))
        );
    }

    #[test]
    fn checked_sub_rejects_underflow() {
        let a = Amount::new(5, 2);
        let b = Amount::new(10, 2);
        assert_eq!(a.checked_sub(b), None);
        assert_eq!(b.checked_sub(a), Some(Amount::new(5, 2)));
    }
}
