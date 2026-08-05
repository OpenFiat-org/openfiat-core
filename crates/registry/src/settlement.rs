//! Paying a fee denominated in one token with a different one — a USDC
//! price settled in OPEN (OFS-1500 §15, OFS-4100 §9.5, OFS-7000 §9/§12).
//!
//! A provider declares exactly one price in exactly one token: the
//! `amount` and `token_mint` on their [`ServicePricing`], and what they
//! declare is overwhelmingly USDC. A payer may hold OPEN instead. Turning
//! the first into the second needs a rate, the rate comes from the oracle,
//! and every hard question in this module follows from the single fact
//! that the rate moves.
//!
//! # Who bears the movement between quote and settlement
//!
//! **The payer, outside a bounded window; the provider, inside it.**
//!
//! The rate has to be fixed at *some* instant, and whichever instant that
//! is, whoever the number is fixed against has written the other party an
//! option. Fix it at settlement and the provider has no idea what they are
//! agreeing to until it arrives. Fix it at quote time — which is what this
//! module does — and the payer holds a free option: they may present the
//! quote or walk away, so they present it only when OPEN has fallen
//! against USDC since, and the provider eats the difference.
//!
//! That option cannot be removed, only bounded. It is bounded: a quote is
//! live for [`MAX_QUOTE_VALIDITY`] at the very most, and for less than
//! that whenever the oracle record behind it lapses sooner — the
//! `expires_at` on [`FeeQuote::Settleable`] is the *earlier* of the two.
//! Past that instant there is no quote at all and the payer must ask for a
//! new one at the new rate, so every movement outside the window lands on
//! them.
//!
//! The cap is not decoration. Without it the window would be whatever
//! `expires_at` the market-data provider happened to publish, and a
//! provider publishing week-long records would be handing every payer on
//! the network a week-long option on OPEN. The feed may shorten this
//! window and may never lengthen it.
//!
//! Positioning is bounded too, from the verifier's own clock rather than
//! from a number the payer wrote: see [`SignedFeeSettlement::accepts`]. A
//! payer who could date a quote a day forward would hold today's rate for
//! a day, which is the same option wearing a different disguise.
//!
//! # When the oracle is stale or absent
//!
//! There is no fallback rate, no last-known value and no default. A
//! [`FeeQuote::Unsettleable`] is a real answer meaning *this fee cannot be
//! settled in that token right now* — the fee itself is untouched and
//! remains payable in the token the provider actually declared. A
//! `GossipOnly` node that holds no oracle records at all, and any node
//! during a feed outage, answers exactly that.
//!
//! `openfiat_oracles` already refuses an expired record to every reader,
//! and [`SettlementRate`] carries that refusal through rather than
//! collapsing it: "nobody publishes USDC/OPEN" and "everybody who does has
//! lapsed" are a missing integration and a broken feed respectively, and a
//! payer should wait for the second and never for the first.
//!
//! # Rounding is upward, deliberately
//!
//! Base units do not divide evenly, and the residue has to go somewhere.
//! It goes to the provider: the settlement amount is the **ceiling**, so a
//! fee settled in a substitute token is never worth less than the fee that
//! was declared, and the payer overpays by strictly less than one base
//! unit of the settlement token.
//!
//! This is deliberately *not* the half-to-even rule
//! `openfiat_advertisements::pricing` uses for a trade price, and the
//! difference is not an oversight. A trade price is bilateral and
//! negotiable — a merchant who lost the residue every time would simply
//! widen their premium to recover it, so directional rounding there only
//! relocates the cost somewhere less visible. A fee is neither. The
//! provider declared one number and has no premium to widen, so rounding
//! down would be a systematic under-collection they cannot price around.
//! And the asymmetry that settles it: substituting the token is the
//! *payer's* election. They may always pay the declared fee in the
//! declared token exactly, so the party who chooses the conversion is the
//! party who carries its residue.
//!
//! # Nothing here credits an earnings statement
//!
//! A [`SignedFeeSettlement`] is a priced commitment, not a receipt. It
//! says a payer agreed to owe a provider a specific number of settlement
//! tokens; it does not say the tokens moved, and no node can tell from the
//! signature whether they did. Crediting [`crate::EarningsLedger`] off one
//! would let anybody inflate any provider's statement by signing
//! statements to themselves. Crediting waits on a settled on-chain
//! transfer, which is not this module's half of the problem.

use crate::pricing::ServicePricing;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{Amount, ErrorCode, PeerId, PublicKey, ServiceId, Signature, Timestamp};
use std::fmt;
use std::time::Duration;

/// The longest a fee quote may be honoured, however long the oracle record
/// behind it says it is good for.
///
/// This is the free option the payer holds — see the module doc — so it is
/// sized to be long enough for a human to approve a wallet prompt and
/// short enough that OPEN cannot meaningfully move inside it.
/// `[PROPOSED — NEEDS SIGN-OFF]` two minutes.
pub const MAX_QUOTE_VALIDITY: Duration = Duration::from_secs(120);

/// How far ahead of the verifying node's own clock a quote may still be
/// live and be accepted.
///
/// Deliberately tighter than `openfiat_reservations`' five minutes, for a
/// reason specific to what it guards here: that constant protects a
/// thirty-minute window, where five minutes is a rounding error. This one
/// protects [`MAX_QUOTE_VALIDITY`], and a five-minute allowance would be
/// two and a half times the window itself — the skew tolerance would
/// become the option rather than bound it.
///
/// A minute still clears ordinary disagreement between unsynchronised
/// machines. Two nodes can differ about a quote signed exactly on the
/// boundary; that is the same bounded edge every clock-checked path in
/// this workspace already accepts, not a new kind of disagreement.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(60);

/// An oracle read of the settlement token's price in the fee's token, or
/// why there isn't one.
///
/// The caller performs the lookup. This crate deliberately does not depend
/// on `openfiat_oracles` — that crate depends on *this* one, since only a
/// registered market-data provider may publish — so the read arrives as a
/// value and the conversion below is a pure function of it. That is the
/// same shape `openfiat_advertisements::pricing::MidPrice` has, for the
/// same reason: two nodes handed the same read must produce the same
/// number, or nothing computed from it could be checked by anyone else.
///
/// There is no "last known rate" variant, on purpose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettlementRate {
    /// How many settlement-token units one fee-token unit buys — for a
    /// USDC fee settled in OPEN, OPEN per USDC — good until `expires_at`.
    Available { rate: f64, expires_at: Timestamp },
    /// No provider publishes this pair.
    NoOracleData,
    /// The pair is published but every record for it has expired.
    StaleOracleData,
}

/// Why a fee cannot be settled in the requested token at this instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UnsettleableReason {
    NoOracleData,
    StaleOracleData,
    /// This build cannot say how many base units one of the settlement
    /// token is, so it cannot produce an [`Amount`] in it. Refused rather
    /// than assumed: a guessed exponent misprices by a factor of a
    /// thousand, silently and plausibly.
    UnknownSettlementToken,
    /// The oracle read was not a usable price — not finite, or zero or
    /// negative. A fee that converts to nothing is not the fee the
    /// provider declared.
    RateOutOfRange,
    /// The converted fee does not fit in an [`Amount`]. Refused rather
    /// than saturated, for the reason
    /// `openfiat_advertisements::pricing::UnpriceableReason::PriceOutOfRange`
    /// gives: a clamped number is one nobody agreed to.
    AmountOutOfRange,
}

/// What a service's fee costs in the settlement token at one instant, or
/// why it has no such price.
///
/// The four outcomes are tagged rather than collapsed into an
/// `Option<Amount>` because a client must render them differently: nothing
/// to pay, pay the declared amount as-is, pay this converted amount before
/// it lapses, and cannot be paid in that token right now.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum FeeQuote {
    /// The service declared no price. OFS-1500 §15 keeps pricing optional
    /// and an absent price already means free, so there is nothing to
    /// convert and nothing to owe.
    Free,
    /// The service already bills in the settlement token. No oracle is
    /// consulted, so a dead feed can never make this fee unpayable — the
    /// same exemption a fixed-price advertisement gets.
    #[serde(rename_all = "camelCase")]
    Native { fee: Amount },
    #[serde(rename_all = "camelCase")]
    Settleable {
        /// The fee exactly as the provider declared it, unconverted.
        fee: Amount,
        fee_mint: String,
        settlement_mint: String,
        /// The declared fee in settlement-token base units, rounded up.
        settlement_amount: Amount,
        /// Settlement-token units per fee-token unit, as read.
        rate: f64,
        quoted_at: Timestamp,
        /// The earlier of the oracle record's own expiry and `quoted_at`
        /// plus [`MAX_QUOTE_VALIDITY`]. Past this there is no quote, and
        /// the payer bears whatever the rate did.
        expires_at: Timestamp,
    },
    #[serde(rename_all = "camelCase")]
    Unsettleable { reason: UnsettleableReason },
}

impl FeeQuote {
    /// What the payer owes in the settlement token, if anything is owed
    /// and it can be priced. Never a zero standing in for "no answer".
    pub const fn settlement_amount(&self) -> Option<Amount> {
        match self {
            Self::Native { fee } => Some(*fee),
            Self::Settleable {
                settlement_amount, ..
            } => Some(*settlement_amount),
            Self::Free | Self::Unsettleable { .. } => None,
        }
    }
}

impl ServicePricing {
    /// Price this fee in `settlement_mint` against one oracle read.
    ///
    /// `settlement_decimals` is the settlement token's own base-unit
    /// exponent and is never guessed here: a caller that cannot establish
    /// it must report [`UnsettleableReason::UnknownSettlementToken`]
    /// rather than assume one.
    pub fn settle_in(
        &self,
        settlement_mint: &str,
        settlement_decimals: u8,
        rate: SettlementRate,
        now: Timestamp,
    ) -> FeeQuote {
        let unsettleable = |reason| FeeQuote::Unsettleable { reason };
        if settlement_mint == self.token_mint {
            return FeeQuote::Native { fee: self.amount };
        }
        let (rate, feed_expires_at) = match rate {
            SettlementRate::Available { rate, expires_at } => (rate, expires_at),
            SettlementRate::NoOracleData => return unsettleable(UnsettleableReason::NoOracleData),
            SettlementRate::StaleOracleData => {
                return unsettleable(UnsettleableReason::StaleOracleData);
            }
        };
        if !rate.is_finite() || rate <= 0.0 {
            return unsettleable(UnsettleableReason::RateOutOfRange);
        }

        // The feed may shorten the window and may never lengthen it.
        let expires_at = Timestamp::from_millis(
            feed_expires_at.as_millis().min(
                now.as_millis()
                    .saturating_add(MAX_QUOTE_VALIDITY.as_millis() as u64),
            ),
        );
        if expires_at.as_millis() <= now.as_millis() {
            // A record handed in already lapsed. `openfiat_oracles` would
            // not have returned one, but this is a pure function of its
            // argument and must not quote off a dead rate because a caller
            // mislabelled it.
            return unsettleable(UnsettleableReason::StaleOracleData);
        }

        match convert(self.amount, rate, settlement_decimals) {
            Some(settlement_amount) => FeeQuote::Settleable {
                fee: self.amount,
                fee_mint: self.token_mint.clone(),
                settlement_mint: settlement_mint.to_string(),
                settlement_amount,
                rate,
                quoted_at: now,
                expires_at,
            },
            None => unsettleable(UnsettleableReason::AmountOutOfRange),
        }
    }
}

/// A declared fee into settlement-token base units, rounded **up**.
///
/// See the module doc for why the residue goes to the provider rather than
/// being split half-to-even the way a trade price is.
///
/// The arithmetic is `f64` because the rate is an `f64` the whole way from
/// the oracle record that published it. Converting it to fixed point here
/// would manufacture precision the feed never had, and would only move the
/// rounding decision one step earlier while making it invisible.
///
/// Returns `None` for anything an [`Amount`] cannot hold.
fn convert(fee: Amount, rate: f64, settlement_decimals: u8) -> Option<Amount> {
    let scale = 10f64.powi(i32::from(settlement_decimals) - i32::from(fee.decimals()));
    let exact = fee.base_units() as f64 * rate * scale;
    if !exact.is_finite() || exact < 0.0 {
        return None;
    }
    let rounded = exact.ceil();
    // `u64::MAX` is not exactly representable as an `f64`, so comparing
    // against it directly would let values just under it through and then
    // wrap on the cast. `2^64` is exact, and `<` against it is not.
    if rounded >= 18_446_744_073_709_551_616.0 {
        return None;
    }
    Some(Amount::new(rounded as u64, settlement_decimals))
}

/// A payer's commitment to settle one service fee in a substitute token at
/// a rate they recorded.
///
/// Every number a verifier needs is on the statement, which is what makes
/// it checkable by a node that has never seen the oracle record behind it
/// — see [`SignedFeeSettlement::accepts`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FeeSettlement {
    pub service_id: ServiceId,
    pub payer: PeerId,
    pub payer_public_key: PublicKey,
    /// The fee as the provider declared it. Restated rather than looked
    /// up, so the statement says which price was being settled even after
    /// the provider re-registers at a different one.
    pub fee: Amount,
    pub fee_mint: String,
    pub settlement_mint: String,
    pub settlement_amount: Amount,
    /// The rate the payer read, in settlement-token units per fee-token
    /// unit. Recorded, never re-derived — see
    /// [`SignedFeeSettlement::accepts`].
    pub rate: f64,
    pub quoted_at: Timestamp,
    pub expires_at: Timestamp,
    /// Which unit of work this settles — the same opaque provenance
    /// [`crate::EarningEntry`] carries, because only the billing role
    /// knows what identifies its own.
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedFeeSettlement {
    pub settlement: FeeSettlement,
    pub signature: Signature,
}

impl SignedFeeSettlement {
    pub fn sign(settlement: FeeSettlement, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::FEE_SETTLEMENT,
            &settlement,
        )
        .expect("FeeSettlement always serializes");
        Self {
            signature: keypair.sign(&bytes),
            settlement,
        }
    }

    /// Verify the signature and that the claimed payer Peer ID really
    /// derives from the claimed public key — the same peer-poisoning
    /// defence [`crate::SignedRegistration`] applies.
    pub fn verify(&self) -> Result<(), FeeSettlementError> {
        let expected = peer_id_from_public_key(&self.settlement.payer_public_key)
            .map_err(|_| FeeSettlementError::InvalidSignature)?;
        if expected != self.settlement.payer {
            return Err(FeeSettlementError::UnauthorizedPayer);
        }
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::FEE_SETTLEMENT,
            &self.settlement,
        )
        .map_err(|_| FeeSettlementError::InvalidSignature)?;
        verify(&self.settlement.payer_public_key, &bytes, &self.signature)
            .map_err(|_| FeeSettlementError::InvalidSignature)
    }

    /// Whether every node should honour this commitment at `now`.
    ///
    /// # Why this checks arithmetic and not the node's own oracle
    ///
    /// It would be wrong to compare `rate` against what this node's oracle
    /// index says. Two honest nodes hold different records — different
    /// providers have reached them, at different times, with different
    /// expiries — so a node that checked against its own view would accept
    /// or refuse the same signed statement depending on which records
    /// happened to arrive first. The same payer would succeed on one
    /// access node and fail on the next, which is precisely the failure
    /// `openfiat_reservations::ReservationRegistry::apply_request` refuses
    /// to introduce for a reservation's agreed price.
    ///
    /// What every node *can* agree on, holding no oracle records at all,
    /// is that the settlement amount follows from the fee the provider
    /// registered and the rate the payer recorded, and that the window the
    /// payer claimed is one this protocol permits. That catches a
    /// miscomputing client and it catches a party later claiming the
    /// conversion produced something else. Whether the recorded rate was
    /// itself honest is a dispute question, and answerable: oracle records
    /// are signed, replicated and timestamped, so an arbitrator can go and
    /// look.
    ///
    /// The one thing this cannot check is the settlement token's real
    /// scale, which is why it does not try. `settlement_amount` carries
    /// its own `decimals`, and the value it denotes is invariant under
    /// that claim — base units scale with the exponent — so a wrong scale
    /// mislabels the amount without ever underpaying it. Whether the label
    /// matches the mint is for the party actually receiving the transfer.
    pub fn accepts(
        &self,
        pricing: Option<&ServicePricing>,
        now: Timestamp,
    ) -> Result<(), FeeSettlementError> {
        self.verify()?;
        let statement = &self.settlement;
        let Some(pricing) = pricing else {
            return Err(FeeSettlementError::ServiceNotPriced);
        };
        if pricing.token_mint != statement.fee_mint
            || pricing.amount.base_units() != statement.fee.base_units()
            || pricing.amount.decimals() != statement.fee.decimals()
        {
            return Err(FeeSettlementError::FeeDisagreement);
        }

        // Two independent bounds on the option the payer holds. The first
        // stops a long self-declared window; the second stops a short
        // window dated forward, which would hold today's rate open until
        // whenever the payer chose to present it.
        let window = statement
            .expires_at
            .as_millis()
            .checked_sub(statement.quoted_at.as_millis())
            .ok_or(FeeSettlementError::QuoteWindowTooLong)?;
        if window > MAX_QUOTE_VALIDITY.as_millis() as u64
            || statement.expires_at.as_millis()
                > now
                    .as_millis()
                    .saturating_add(MAX_QUOTE_VALIDITY.as_millis() as u64)
                    .saturating_add(MAX_CLOCK_SKEW.as_millis() as u64)
        {
            return Err(FeeSettlementError::QuoteWindowTooLong);
        }
        if now.as_millis() >= statement.expires_at.as_millis() {
            return Err(FeeSettlementError::QuoteExpired);
        }

        // Recomputed through the same path that produced the quote, so the
        // check and the quote can never disagree about rounding — the
        // failure a separately-written check drifts into on exactly the
        // boundary cases the rounding rule exists to settle.
        match pricing.settle_in(
            &statement.settlement_mint,
            statement.settlement_amount.decimals(),
            SettlementRate::Available {
                rate: statement.rate,
                expires_at: statement.expires_at,
            },
            statement.quoted_at,
        ) {
            FeeQuote::Settleable {
                settlement_amount,
                expires_at,
                ..
            } if settlement_amount == statement.settlement_amount
                && expires_at == statement.expires_at =>
            {
                Ok(())
            }
            _ => Err(FeeSettlementError::PriceDisagreement),
        }
    }
}

/// Why a signed fee settlement is not one a node will honour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeSettlementError {
    InvalidSignature,
    /// The claimed payer Peer ID does not derive from the claimed key.
    UnauthorizedPayer,
    /// The service declares no price, so there is no fee to settle. An
    /// absent price means free (OFS-1500 §15), and a settlement against
    /// one is a claim about a charge that was never made.
    ServiceNotPriced,
    /// The restated fee is not the one this service registered.
    FeeDisagreement,
    /// The settlement amount does not follow from the fee and the rate the
    /// payer themselves recorded.
    PriceDisagreement,
    /// The quote's window is longer than [`MAX_QUOTE_VALIDITY`], or is
    /// positioned far enough ahead of this node's clock to be the same
    /// thing by another route.
    QuoteWindowTooLong,
    /// The quote was well-formed and its window has passed. The fee is
    /// still owed; it needs re-quoting at the current rate.
    QuoteExpired,
}

impl FeeSettlementError {
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::InvalidSignature => ErrorCode::InvalidSignature,
            Self::UnauthorizedPayer => ErrorCode::InvalidIdentityClaim,
            Self::ServiceNotPriced
            | Self::FeeDisagreement
            | Self::PriceDisagreement
            | Self::QuoteWindowTooLong => ErrorCode::InvalidRequest,
            // The quote's validity window has passed, so the signed
            // settlement is a stale artifact — the same shape as
            // `WalletError::RequestExpired`, and the same code. Not
            // `SessionExpired` (1006), where this used to land: a payer
            // told their session expired re-authenticates and re-sends
            // the same settlement, carrying the same expired quote. The
            // fee is still owed; what has to change is the rate it is
            // quoted at.
            Self::QuoteExpired => ErrorCode::RequestExpired,
        }
    }
}

impl fmt::Display for FeeSettlementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code().name())
    }
}

impl std::error::Error for FeeSettlementError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::BillingUnit;

    const USDC: &str = "2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU";
    const OPEN: &str = "29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj";
    /// OPEN is a Token-2022 mint with nine decimals.
    const OPEN_DECIMALS: u8 = 9;

    /// A fee in USDC, the token a provider overwhelmingly declares.
    fn fee(base_units: u64) -> ServicePricing {
        ServicePricing {
            token_mint: USDC.to_string(),
            amount: Amount::new(base_units, 6),
            unit: BillingUnit::Request,
        }
    }

    fn live(rate: f64) -> SettlementRate {
        SettlementRate::Available {
            rate,
            expires_at: Timestamp::from_millis(60_000),
        }
    }

    #[test]
    fn a_usdc_fee_converts_into_open_at_the_published_rate() {
        // 2.50 USDC at 4 OPEN per USDC is 10 OPEN, which at nine decimals
        // is 10_000_000_000 base units.
        let quote = fee(2_500_000).settle_in(
            OPEN,
            OPEN_DECIMALS,
            live(4.0),
            Timestamp::from_millis(1_000),
        );
        assert_eq!(
            quote.settlement_amount(),
            Some(Amount::new(10_000_000_000, 9))
        );
        assert_eq!(
            quote.settlement_amount().unwrap().to_string(),
            "10.000000000"
        );
    }

    #[test]
    fn a_residue_always_favours_the_provider() {
        // Rounding down would under-collect on every settlement, and the
        // provider has no premium to widen to recover it. Half-to-even
        // would give 1 for the first case below — a payer settling one
        // base unit short, systematically.
        let quote = |base_units, rate| {
            ServicePricing {
                token_mint: USDC.to_string(),
                amount: Amount::new(base_units, 6),
                unit: BillingUnit::Request,
            }
            .settle_in(OPEN, 6, live(rate), Timestamp::from_millis(1_000))
            .settlement_amount()
            .unwrap()
            .base_units()
        };
        assert_eq!(quote(1, 1.5), 2, "half a base unit rounds up, not to even");
        assert_eq!(quote(1, 1.1), 2, "any residue at all rounds up");
        assert_eq!(quote(1, 1.0), 1, "an exact conversion gains nothing");
    }

    #[test]
    fn the_payer_never_overpays_by_a_whole_base_unit() {
        // The bound the ceiling rule promises, from both sides: never
        // less than the declared fee, never a whole base unit more.
        for base_units in [1u64, 7, 999, 2_500_000] {
            for rate in [0.000_37_f64, 1.5, 4.0, 129.52] {
                let exact = base_units as f64 * rate * 10f64.powi(3);
                let settled = fee(base_units)
                    .settle_in(OPEN, 9, live(rate), Timestamp::from_millis(1_000))
                    .settlement_amount()
                    .unwrap()
                    .base_units() as f64;
                assert!(settled >= exact, "{base_units} at {rate} underpaid");
                assert!(
                    settled - exact < 1.0,
                    "{base_units} at {rate} overpaid by a whole base unit"
                );
            }
        }
    }

    #[test]
    fn a_fee_already_in_the_settlement_token_needs_no_oracle_at_all() {
        // The exemption a fixed-price advertisement gets: a dead feed must
        // never make a fee unpayable that never depended on a feed.
        let quote = ServicePricing {
            token_mint: OPEN.to_string(),
            amount: Amount::new(5_000_000_000, 9),
            unit: BillingUnit::Month,
        }
        .settle_in(
            OPEN,
            OPEN_DECIMALS,
            SettlementRate::NoOracleData,
            Timestamp::from_millis(1_000),
        );
        assert_eq!(
            quote,
            FeeQuote::Native {
                fee: Amount::new(5_000_000_000, 9)
            }
        );
    }

    /// The failure this whole module exists to prevent: a lapsed feed must
    /// not settle a fee at yesterday's rate.
    #[test]
    fn a_stale_feed_leaves_the_fee_unsettleable_rather_than_priced_at_the_last_rate() {
        let quote = fee(2_500_000).settle_in(
            OPEN,
            OPEN_DECIMALS,
            SettlementRate::StaleOracleData,
            Timestamp::from_millis(1_000),
        );
        assert_eq!(
            quote,
            FeeQuote::Unsettleable {
                reason: UnsettleableReason::StaleOracleData
            }
        );
        assert_eq!(
            quote.settlement_amount(),
            None,
            "a lapsed feed must not yield a number"
        );
    }

    #[test]
    fn a_pair_nobody_publishes_is_a_different_answer_from_a_lapsed_one() {
        // Stale means the feed will likely come back and waiting is
        // sensible; NoData means nobody prices this at all and waiting is
        // pointless. A payer must not be shown the same thing for both.
        assert_eq!(
            fee(2_500_000).settle_in(
                OPEN,
                OPEN_DECIMALS,
                SettlementRate::NoOracleData,
                Timestamp::from_millis(1_000)
            ),
            FeeQuote::Unsettleable {
                reason: UnsettleableReason::NoOracleData
            }
        );
    }

    #[test]
    fn a_record_handed_in_already_lapsed_is_refused_rather_than_honoured() {
        let quote = fee(2_500_000).settle_in(
            OPEN,
            OPEN_DECIMALS,
            SettlementRate::Available {
                rate: 4.0,
                expires_at: Timestamp::from_millis(1_000),
            },
            Timestamp::from_millis(1_000),
        );
        assert_eq!(
            quote,
            FeeQuote::Unsettleable {
                reason: UnsettleableReason::StaleOracleData
            }
        );
    }

    #[test]
    fn a_rate_that_is_not_a_price_is_refused() {
        for rate in [0.0, -4.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                fee(2_500_000).settle_in(
                    OPEN,
                    OPEN_DECIMALS,
                    live(rate),
                    Timestamp::from_millis(1_000)
                ),
                FeeQuote::Unsettleable {
                    reason: UnsettleableReason::RateOutOfRange
                },
                "{rate} is not a price"
            );
        }
    }

    #[test]
    fn a_conversion_too_large_for_an_amount_is_refused_rather_than_wrapping() {
        assert_eq!(
            fee(u64::MAX).settle_in(
                OPEN,
                OPEN_DECIMALS,
                live(1e30),
                Timestamp::from_millis(1_000)
            ),
            FeeQuote::Unsettleable {
                reason: UnsettleableReason::AmountOutOfRange
            }
        );
    }

    /// The feed may shorten the window and may never lengthen it —
    /// otherwise the option a payer holds is whatever `expires_at` a
    /// market-data provider felt like publishing.
    #[test]
    fn a_long_lived_oracle_record_does_not_buy_a_long_lived_quote() {
        let a_week = Timestamp::from_millis(7 * 24 * 60 * 60 * 1_000);
        let quote = fee(2_500_000).settle_in(
            OPEN,
            OPEN_DECIMALS,
            SettlementRate::Available {
                rate: 4.0,
                expires_at: a_week,
            },
            Timestamp::from_millis(1_000),
        );
        let FeeQuote::Settleable { expires_at, .. } = quote else {
            panic!("a live record must produce a quote");
        };
        assert_eq!(
            expires_at,
            Timestamp::from_millis(1_000 + MAX_QUOTE_VALIDITY.as_millis() as u64)
        );
    }

    #[test]
    fn a_short_lived_oracle_record_shortens_the_quote_with_it() {
        let quote = fee(2_500_000).settle_in(
            OPEN,
            OPEN_DECIMALS,
            SettlementRate::Available {
                rate: 4.0,
                expires_at: Timestamp::from_millis(6_000),
            },
            Timestamp::from_millis(1_000),
        );
        let FeeQuote::Settleable { expires_at, .. } = quote else {
            panic!("a live record must produce a quote");
        };
        assert_eq!(expires_at, Timestamp::from_millis(6_000));
    }

    /// Two nodes handed the same read must produce the same number — the
    /// property that makes a quote safe to commit to at all.
    #[test]
    fn conversion_is_deterministic_for_a_given_read() {
        let at = Timestamp::from_millis(1_000);
        assert_eq!(
            fee(2_500_000).settle_in(OPEN, 9, live(4.123_456_789), at),
            fee(2_500_000).settle_in(OPEN, 9, live(4.123_456_789), at)
        );
    }

    mod commitments {
        use super::*;
        use openfiat_crypto::Keypair;

        fn signed_at(
            pricing: &ServicePricing,
            quoted_at: Timestamp,
            keypair: &Keypair,
        ) -> SignedFeeSettlement {
            let FeeQuote::Settleable {
                fee,
                fee_mint,
                settlement_mint,
                settlement_amount,
                rate,
                quoted_at,
                expires_at,
            } = pricing.settle_in(
                OPEN,
                OPEN_DECIMALS,
                SettlementRate::Available {
                    rate: 4.0,
                    expires_at: Timestamp::from_millis(quoted_at.as_millis() + 30_000),
                },
                quoted_at,
            )
            else {
                panic!("a live record must produce a quote");
            };
            SignedFeeSettlement::sign(
                FeeSettlement {
                    service_id: ServiceId::new("svc-1"),
                    payer: peer_id_from_public_key(&keypair.public_key()).unwrap(),
                    payer_public_key: keypair.public_key(),
                    fee,
                    fee_mint,
                    settlement_mint,
                    settlement_amount,
                    rate,
                    quoted_at,
                    expires_at,
                    reference: "delivery-1".to_string(),
                },
                keypair,
            )
        }

        #[test]
        fn a_commitment_that_follows_from_the_declared_fee_is_accepted() {
            let payer = Keypair::generate();
            let pricing = fee(2_500_000);
            let signed = signed_at(&pricing, Timestamp::from_millis(1_000), &payer);
            assert_eq!(
                signed.accepts(Some(&pricing), Timestamp::from_millis(2_000)),
                Ok(())
            );
        }

        #[test]
        fn a_settlement_amount_that_does_not_follow_from_its_own_rate_is_refused() {
            // The right rate, the wrong arithmetic — a miscomputing
            // client, or a payer quietly paying a tenth of what they owe.
            let payer = Keypair::generate();
            let pricing = fee(2_500_000);
            let mut signed = signed_at(&pricing, Timestamp::from_millis(1_000), &payer);
            signed.settlement.settlement_amount = Amount::new(1_000_000_000, 9);
            let signed = SignedFeeSettlement::sign(signed.settlement, &payer);
            assert_eq!(
                signed.accepts(Some(&pricing), Timestamp::from_millis(2_000)),
                Err(FeeSettlementError::PriceDisagreement)
            );
        }

        #[test]
        fn a_commitment_against_a_fee_the_service_never_declared_is_refused() {
            let payer = Keypair::generate();
            let signed = signed_at(&fee(2_500_000), Timestamp::from_millis(1_000), &payer);
            assert_eq!(
                signed.accepts(Some(&fee(9_900_000)), Timestamp::from_millis(2_000)),
                Err(FeeSettlementError::FeeDisagreement)
            );
        }

        #[test]
        fn a_commitment_against_a_free_service_is_refused() {
            // An absent price already means free, so a settlement against
            // one is a claim about a charge that was never made.
            let payer = Keypair::generate();
            let signed = signed_at(&fee(2_500_000), Timestamp::from_millis(1_000), &payer);
            assert_eq!(
                signed.accepts(None, Timestamp::from_millis(2_000)),
                Err(FeeSettlementError::ServiceNotPriced)
            );
        }

        #[test]
        fn a_tampered_commitment_is_refused() {
            let payer = Keypair::generate();
            let pricing = fee(2_500_000);
            let mut signed = signed_at(&pricing, Timestamp::from_millis(1_000), &payer);
            signed.settlement.rate = 40.0;
            assert_eq!(
                signed.accepts(Some(&pricing), Timestamp::from_millis(2_000)),
                Err(FeeSettlementError::InvalidSignature)
            );
        }

        #[test]
        fn a_payer_id_that_does_not_derive_from_its_key_is_refused() {
            let payer = Keypair::generate();
            let pricing = fee(2_500_000);
            let mut signed = signed_at(&pricing, Timestamp::from_millis(1_000), &payer);
            signed.settlement.payer = PeerId::from_bytes(vec![0, 0, 0]);
            let signed = SignedFeeSettlement::sign(signed.settlement, &payer);
            assert_eq!(
                signed.accepts(Some(&pricing), Timestamp::from_millis(2_000)),
                Err(FeeSettlementError::UnauthorizedPayer)
            );
        }

        #[test]
        fn a_quote_presented_after_its_window_is_refused() {
            // The point of the window: past it the payer bears whatever
            // the rate did, and must re-quote.
            let payer = Keypair::generate();
            let pricing = fee(2_500_000);
            let signed = signed_at(&pricing, Timestamp::from_millis(1_000), &payer);
            let expires_at = signed.settlement.expires_at;
            assert_eq!(
                signed.accepts(Some(&pricing), expires_at),
                Err(FeeSettlementError::QuoteExpired),
                "the boundary instant is already past the window"
            );
        }

        #[test]
        fn a_payer_cannot_write_themselves_a_longer_window() {
            // The option-bounding argument, as an attack: a payer who
            // could declare their own validity would hold a free option
            // on OPEN for as long as they liked.
            let payer = Keypair::generate();
            let pricing = fee(2_500_000);
            let mut signed = signed_at(&pricing, Timestamp::from_millis(1_000), &payer);
            signed.settlement.expires_at = Timestamp::from_millis(
                signed.settlement.quoted_at.as_millis() + MAX_QUOTE_VALIDITY.as_millis() as u64 + 1,
            );
            let signed = SignedFeeSettlement::sign(signed.settlement, &payer);
            assert_eq!(
                signed.accepts(Some(&pricing), Timestamp::from_millis(2_000)),
                Err(FeeSettlementError::QuoteWindowTooLong)
            );
        }

        #[test]
        fn a_payer_cannot_date_a_short_window_far_enough_ahead_to_be_a_long_one() {
            // The same option by another route: a two-minute window
            // starting tomorrow holds today's rate until tomorrow.
            let payer = Keypair::generate();
            let pricing = fee(2_500_000);
            let a_day = 24 * 60 * 60 * 1_000;
            let signed = signed_at(&pricing, Timestamp::from_millis(a_day), &payer);
            assert_eq!(
                signed.accepts(Some(&pricing), Timestamp::from_millis(1_000)),
                Err(FeeSettlementError::QuoteWindowTooLong)
            );
        }

        /// The divergence property at the unit level: the verdict is a
        /// pure function of the statement and the registration, so it
        /// cannot consult an oracle even if a future edit wanted it to.
        #[test]
        fn the_verdict_reads_nothing_but_the_statement_and_the_declared_fee() {
            let payer = Keypair::generate();
            let pricing = fee(2_500_000);
            // A rate no oracle on the network would agree with. It is
            // still internally consistent, so it is still accepted — the
            // honesty of the rate is a dispute question, not a node's.
            let FeeQuote::Settleable {
                fee: declared,
                fee_mint,
                settlement_mint,
                settlement_amount,
                rate,
                quoted_at,
                expires_at,
            } = pricing.settle_in(
                OPEN,
                OPEN_DECIMALS,
                SettlementRate::Available {
                    rate: 999.0,
                    expires_at: Timestamp::from_millis(31_000),
                },
                Timestamp::from_millis(1_000),
            )
            else {
                panic!("a live record must produce a quote");
            };
            let signed = SignedFeeSettlement::sign(
                FeeSettlement {
                    service_id: ServiceId::new("svc-1"),
                    payer: peer_id_from_public_key(&payer.public_key()).unwrap(),
                    payer_public_key: payer.public_key(),
                    fee: declared,
                    fee_mint,
                    settlement_mint,
                    settlement_amount,
                    rate,
                    quoted_at,
                    expires_at,
                    reference: "delivery-1".to_string(),
                },
                &payer,
            );
            assert_eq!(
                signed.accepts(Some(&pricing), Timestamp::from_millis(2_000)),
                Ok(())
            );
        }
    }
}
