//! What a provider charges, in a form something can actually bill against
//! (OFS-4100 §9.5).
//!
//! OFS-1500 §15 leaves pricing optional and says nothing about its shape,
//! so this field started life as free text. Free text advertises a price
//! to a human and is useless to a program: there is no way to know which
//! token "0.001 per call" is denominated in, nor what a "call" is. A
//! provider billing in OPEN, USDC, or any other configured token needs the
//! token to be named unambiguously, which on Solana means the mint.

use openfiat_types::Amount;

/// What a single charge covers. Providers meter different things — a
/// notification is delivered once, a snapshot is served once, a risk
/// screening answers one question — so the unit travels with the price
/// rather than being assumed per role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BillingUnit {
    /// One served request: a delivered notification, a served snapshot,
    /// one screening answered.
    Request,
    /// One completed trade the service took part in.
    Trade,
    /// A calendar month of access, however much it is used.
    Month,
}

/// A provider's declared price.
///
/// Declaring a price is not the same as being able to collect it. Per
/// OFS-4100 §9.5 the billing *trigger* — who pays and at which moment —
/// is deliberately unsettled and differs by role, so a provider can
/// publish this today and no protocol path will yet charge anyone against
/// it. That gap is stated rather than hidden: see
/// [`crate::earnings::EarningsLedger`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServicePricing {
    /// Base58 SPL mint address of the token billed in. The mint *is* the
    /// token's identity — a symbol like "USDC" is ambiguous across
    /// clusters and spoofable, a mint address is neither.
    pub token_mint: String,
    /// Price for one `unit`, as a base-unit count plus the token's own
    /// decimal precision. Never a float, matching every other amount in
    /// this workspace.
    pub amount: Amount,
    pub unit: BillingUnit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_serialization::wire;

    #[test]
    fn a_price_survives_the_wire_format() {
        let price = ServicePricing {
            token_mint: "29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj".to_string(),
            amount: Amount::new(1_000_000, 9),
            unit: BillingUnit::Request,
        };
        let bytes = wire::to_bytes(&price).unwrap();
        assert_eq!(wire::from_bytes::<ServicePricing>(&bytes).unwrap(), price);
    }

    /// The live devnet cluster carries 9 registrations, every one of them
    /// with `pricing: null`. postcard encodes `Option::None` as a single
    /// zero byte with no reference to the inner type, so widening
    /// `Option<String>` to `Option<ServicePricing>` leaves those records
    /// decodable. This pins that, because it is the entire reason the
    /// change did not need a migration.
    #[test]
    fn none_encodes_identically_whatever_the_inner_type_is() {
        let as_text: Option<String> = None;
        let as_price: Option<ServicePricing> = None;
        assert_eq!(
            wire::to_bytes(&as_text).unwrap(),
            wire::to_bytes(&as_price).unwrap()
        );
        // And an old `None` really does decode as the new type.
        let old_bytes = wire::to_bytes(&as_text).unwrap();
        assert_eq!(
            wire::from_bytes::<Option<ServicePricing>>(&old_bytes).unwrap(),
            None
        );
    }
}
