//! What the network has actually moved, per asset.
//!
//! # Only settled, only confirmed
//!
//! A settlement counts here when it carries an `escrow_release_signature`
//! — that is, when this node independently observed the on-chain release
//! confirm. Everything looser measures intent rather than volume: a
//! reservation is a request, an initiated settlement is a trade in
//! progress, and a sum over advertisements is what merchants say they are
//! willing to trade. An explorer whose whole point is that its numbers
//! came from somewhere real cannot report any of those as volume.
//!
//! # Per asset, never summed
//!
//! These are different tokens with different scales. One "total volume"
//! figure would add SOL to USDC, and would do it silently. So the answer
//! is a list, each entry carrying its own mint and decimals — taken from
//! the mint rather than assumed, because USDC and USDT are 6 and wSOL is
//! 9, and a hardcoded 6 would report SOL volume a thousand times too
//! large.
//!
//! # Where the asset comes from, and why some settlements have none
//!
//! A `Settlement` does not record which token it moved. It carries an
//! amount and a reservation id, and the asset is two hops away:
//! reservation → advertisement → `asset_mint`. So this joins, and a
//! settlement whose advertisement has since been deleted (OFS-2100 §21)
//! cannot be attributed to an asset at all.
//!
//! Those are counted and reported rather than dropped. A figure that
//! quietly omits what it could not classify is a figure that looks
//! complete and is not, and the number of unattributed settlements is
//! exactly what tells a reader how much to trust the rest.

use crate::dispatch::{MethodTable, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_storage::KvStore;
use std::collections::HashMap;

/// Everything moved in one asset.
#[derive(Debug, serde::Serialize)]
pub struct AssetVolume {
    /// The mint — the identity. Always present.
    pub asset_mint: String,
    /// What people call it, if this build knows a name. `null` for a mint
    /// with no nickname, which is shown by address rather than guessed at.
    pub asset_symbol: Option<&'static str>,
    /// Scale, from the mint. `null` alongside an unknown symbol: this
    /// node can total the base units either way, but it cannot say where
    /// the decimal point goes without knowing the mint.
    pub decimals: Option<u8>,
    /// Summed in base units, so no rounding happens on the way here.
    pub base_units: u128,
    pub settlements: u64,
}

#[derive(Debug, serde::Serialize)]
pub struct SettledVolume {
    pub assets: Vec<AssetVolume>,
    /// Confirmed settlements whose asset could not be established,
    /// because the advertisement behind them is gone.
    ///
    /// Reported rather than silently excluded — see the module doc.
    pub unattributed_settlements: u64,
    /// Every settlement this node has, confirmed or not. The difference
    /// between this and the counts above is trades in flight, and a
    /// reader comparing them can see how much of the book is settled.
    pub settlements_known: u64,
    /// What this figure is *not*.
    ///
    /// One node reports what it has replicated, which is not necessarily
    /// the network's whole history — a node that joined last week, or one
    /// on a rolling retention window, honestly holds less. Stated in the
    /// response rather than left to a footnote somewhere, because a
    /// number presented without its scope reads as a global total.
    pub scope: &'static str,
}

const SCOPE: &str = "settlements this node has replicated and independently observed confirmed on chain; \
     not necessarily the network's entire history";

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getSettledVolume",
        method_fn(
            |state: &NodeState<S>, _params: serde_json::Value| -> Result<SettledVolume, RpcError> {
                let settlements = state.settlements.all();
                let settlements_known = settlements.len() as u64;
                let mut totals: HashMap<String, (u128, u64)> = HashMap::new();
                let mut unattributed = 0u64;

                for settlement in settlements {
                    // The confirmation is the whole qualification. An
                    // `Approved` settlement whose release has not landed
                    // has moved nothing yet.
                    if settlement.escrow_release_signature.is_none() {
                        continue;
                    }
                    match asset_of(state, &settlement) {
                        Some(mint) => {
                            let entry = totals.entry(mint).or_insert((0, 0));
                            entry.0 = entry
                                .0
                                .saturating_add(u128::from(settlement.amount.base_units()));
                            entry.1 += 1;
                        }
                        None => unattributed += 1,
                    }
                }

                let mut assets: Vec<AssetVolume> = totals
                    .into_iter()
                    .map(|(mint, (base_units, settlements))| {
                        let known = openfiat_crypto::MintAddress::parse(&mint)
                            .ok()
                            .and_then(|m| openfiat_chain::mints::known(&m));
                        AssetVolume {
                            asset_symbol: known.map(|k| k.symbol),
                            decimals: known.map(|k| k.decimals),
                            asset_mint: mint,
                            base_units,
                            settlements,
                        }
                    })
                    .collect();
                // Largest first, then by mint so the order is stable
                // between calls rather than whatever the map iterated.
                assets.sort_by(|a, b| {
                    b.base_units
                        .cmp(&a.base_units)
                        .then_with(|| a.asset_mint.cmp(&b.asset_mint))
                });

                Ok(SettledVolume {
                    assets,
                    unattributed_settlements: unattributed,
                    settlements_known,
                    scope: SCOPE,
                })
            },
        ),
    );
}

/// The mint a settlement was denominated in, by the only route there is.
fn asset_of<S: KvStore + 'static>(
    state: &NodeState<S>,
    settlement: &openfiat_settlement::Settlement,
) -> Option<String> {
    let reservation = state.reservations.get(&settlement.reservation_id)?;
    let advertisement = state.advertisements.get(&reservation.advertisement_id)?;
    Some(advertisement.asset_mint.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::methods::build_table;
    use openfiat_advertisements::events::{AdvertisementCreate, SignedAdvertisementCreate};
    use openfiat_advertisements::{AdvertisementId, Direction, PricingModel};
    use openfiat_crypto::{Keypair, MintAddress};
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_reservations::ReservationId;
    use openfiat_reservations::events::{ReservationRequest, SignedReservationRequest};
    use openfiat_settlement::SettlementId;
    use openfiat_settlement::events::{
        PaymentSubmitted, SettlementApproved, SettlementInitiate, SignedPaymentSubmitted,
        SignedSettlementApproved, SignedSettlementInitiate,
    };
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{Amount, FiatCurrency, PeerId, Timestamp};

    const USDC: &str = "2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU";
    const WSOL: &str = "So11111111111111111111111111111111111111112";

    fn peer(keypair: &Keypair) -> PeerId {
        peer_id_from_public_key(&keypair.public_key()).unwrap()
    }

    /// One advertisement, one reservation against it, one settlement —
    /// the whole chain, because the asset is only reachable through it.
    fn trade(
        state: &NodeState<MemoryStore>,
        suffix: &str,
        mint: &str,
        units: u64,
        decimals: u8,
        confirmed: bool,
    ) {
        let merchant = Keypair::from_seed([1u8; 32]);
        let buyer = Keypair::from_seed([2u8; 32]);
        let price = Amount::new(129_000_000, 6);
        let ad_id = AdvertisementId::new(format!("ad-{suffix}"));

        state
            .advertisements
            .apply_create(SignedAdvertisementCreate::sign(
                AdvertisementCreate {
                    id: ad_id.clone(),
                    merchant: peer(&merchant),
                    merchant_public_key: merchant.public_key(),
                    asset_mint: MintAddress::parse(mint).unwrap(),
                    direction: Direction::Sell,
                    fiat_currency: FiatCurrency::parse("KES").unwrap(),
                    min_trade: Amount::new(1, decimals),
                    max_trade: Amount::new(u64::MAX, decimals),
                    initial_liquidity: Amount::new(u64::MAX, decimals),
                    pricing: PricingModel::Fixed { price },
                    payment_methods: vec![],
                    timestamp: Timestamp::from_millis(1),
                },
                &merchant,
            ))
            .expect("advertisement");

        let reservation_id = ReservationId::new(format!("res-{suffix}"));
        state
            .reservations
            .apply_request(SignedReservationRequest::sign(
                ReservationRequest {
                    id: reservation_id.clone(),
                    advertisement_id: ad_id,
                    requester: peer(&buyer),
                    requester_public_key: buyer.public_key(),
                    amount: Amount::new(units, decimals),
                    agreed_price: price,
                    agreed_mid: None,
                    timestamp: Timestamp::from_millis(2),
                },
                &buyer,
            ))
            .expect("reservation");

        let settlement_id = SettlementId::new(format!("settle-{suffix}"));
        state
            .settlements
            .apply_initiate(SignedSettlementInitiate::sign(
                SettlementInitiate {
                    id: settlement_id.clone(),
                    reservation_id,
                    buyer: peer(&buyer),
                    buyer_public_key: buyer.public_key(),
                    seller: peer(&merchant),
                    seller_public_key: merchant.public_key(),
                    amount: Amount::new(units, decimals),
                    timestamp: Timestamp::from_millis(3),
                },
                &buyer,
            ))
            .expect("settlement");

        if confirmed {
            // A release only follows an approval, so the fixture walks
            // the real state machine rather than jumping to the end. The
            // whole point of this method is that it counts trades that
            // genuinely completed.
            state
                .settlements
                .apply_payment_submitted(SignedPaymentSubmitted::sign(
                    PaymentSubmitted {
                        settlement_id: settlement_id.clone(),
                        buyer: peer(&buyer),
                        payment_reference: None,
                        timestamp: Timestamp::from_millis(4),
                    },
                    &buyer,
                ))
                .expect("payment declared");
            state
                .settlements
                .apply_approved(SignedSettlementApproved::sign(
                    SettlementApproved {
                        settlement_id: settlement_id.clone(),
                        seller: peer(&merchant),
                        timestamp: Timestamp::from_millis(5),
                    },
                    &merchant,
                ))
                .expect("merchant approved");
            state
                .settlements
                .apply_escrow_released(&settlement_id, format!("sig-{suffix}"))
                .expect("an observed on-chain release");
        }
    }

    fn volume(state: &NodeState<MemoryStore>) -> serde_json::Value {
        build_table::<MemoryStore>()
            .dispatch(state, "getSettledVolume", serde_json::json!({}))
            .expect("volume is always answerable")
    }

    #[test]
    fn only_settlements_confirmed_on_chain_count_as_volume() {
        let state = NodeState::new_for_test(MemoryStore::new());
        trade(&state, "a", USDC, 5_000_000, 6, true);
        trade(&state, "b", USDC, 3_000_000, 6, false);

        let result = volume(&state);
        assert_eq!(result["settlements_known"], 2);
        assert_eq!(result["assets"][0]["base_units"], 5_000_000u64);
        assert_eq!(
            result["assets"][0]["settlements"], 1,
            "a trade in progress has moved nothing yet"
        );
    }

    #[test]
    fn assets_are_never_added_together() {
        // The failure this shape prevents: 1 wSOL and 1 USDC are not 2 of
        // anything, and their base units are not even the same scale.
        let state = NodeState::new_for_test(MemoryStore::new());
        trade(&state, "usdc", USDC, 10_000_000, 6, true);
        trade(&state, "sol", WSOL, 2_000_000_000, 9, true);

        let result = volume(&state);
        let assets = result["assets"].as_array().unwrap();
        assert_eq!(assets.len(), 2);

        let sol = assets.iter().find(|a| a["asset_mint"] == WSOL).unwrap();
        assert_eq!(sol["asset_symbol"], "wSOL");
        assert_eq!(
            sol["decimals"], 9,
            "decimals come from the mint — assuming 6 would report SOL a thousand times too large"
        );
        let usdc = assets.iter().find(|a| a["asset_mint"] == USDC).unwrap();
        assert_eq!(usdc["decimals"], 6);
    }

    #[test]
    fn a_settlement_whose_advertisement_is_gone_is_counted_not_hidden() {
        // Advertisements can be deleted, and a settlement then has no
        // route to its asset. Dropping it silently would make the totals
        // look complete while being short.
        let state = NodeState::new_for_test(MemoryStore::new());
        trade(&state, "orphan", USDC, 4_000_000, 6, true);
        // A settlement whose reservation is gone reaches its asset by no
        // route at all — the same shape as a deleted advertisement, and
        // the one this test can produce without a delete API.
        state
            .store
            .delete("reservations", b"res-orphan")
            .expect("the fixture store deletes");

        let result = volume(&state);
        assert_eq!(result["unattributed_settlements"], 1);
        assert!(result["assets"].as_array().unwrap().is_empty());
    }

    #[test]
    fn an_empty_network_reports_zero_rather_than_failing() {
        let state = NodeState::new_for_test(MemoryStore::new());
        let result = volume(&state);
        assert!(result["assets"].as_array().unwrap().is_empty());
        assert_eq!(result["settlements_known"], 0);
        assert!(
            result["scope"].as_str().unwrap().contains("this node"),
            "the figure must never read as a global total"
        );
    }
}
