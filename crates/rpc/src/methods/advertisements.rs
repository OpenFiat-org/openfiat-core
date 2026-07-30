//! Advertisement methods (OFS-2100).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_advertisements::events::{
    SignedAdvertisementCreate, SignedAdvertisementDisable, SignedAdvertisementPriceUpdate,
};
use openfiat_advertisements::pricing::{MidPrice, PriceQuote};
use openfiat_advertisements::protocol;
use openfiat_advertisements::{Advertisement, AdvertisementId, PricingModel};
use openfiat_oracles::ExchangeRateLookup;
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::{Priority, Timestamp};
use std::collections::HashMap;

/// An advertisement as a *reader* sees it: the replicated record exactly
/// as stored, plus the price it resolves to at the instant of this call.
///
/// The quote is attached here rather than stored on the record for the
/// reason [`openfiat_advertisements::pricing`] gives — a resolved price
/// written back onto a gossiped record would be stale between refreshes
/// and different on every node. Flattened, so the record's own fields stay
/// exactly where every existing client already reads them and `quote` is
/// purely additive.
///
/// This is the DISPLAY half of price resolution, and it is honest about
/// being only that: two nodes answering at the same moment may return
/// different `quote`s for the same floating advertisement, because each
/// resolves against the oracle records it has and its own clock. Neither
/// is wrong. That is exactly why the number a taker is held to has to be
/// pinned by the commitment rather than read from here — see the module
/// doc on `mid_expires_at` in [`PriceQuote::Floating`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdvertisementView {
    #[serde(flatten)]
    pub advertisement: Advertisement,
    pub quote: PriceQuote,
}

/// Resolves one advertisement against `oracles`, reading the pair from the
/// advertisement itself (`asset`/`fiat_currency` — the price is fiat per
/// unit of asset, so that is the direction the rate is looked up in).
fn resolve<S: KvStore + 'static>(
    state: &NodeState<S>,
    advertisement: Advertisement,
    now: Timestamp,
    cache: &mut HashMap<(String, String), MidPrice>,
) -> AdvertisementView {
    // A fixed advertisement never consults the oracle, so a dead feed can
    // never make one unpriceable.
    let quote = match &advertisement.pricing {
        PricingModel::Fixed { .. } => advertisement.pricing.quote(MidPrice::NoOracleData),
        PricingModel::Floating { .. } => {
            let pair = (
                advertisement.asset.clone(),
                advertisement.fiat_currency.clone(),
            );
            // Each lookup is a full scan of the oracle column family, so a
            // book with many advertisements on one pair would otherwise
            // rescan it once per row. Caching per call also guarantees
            // every row in one response is priced off the *same* read,
            // rather than off a feed that lapsed midway through.
            let mid = *cache.entry(pair).or_insert_with(|| {
                match state.oracles.exchange_rate(
                    &advertisement.asset,
                    &advertisement.fiat_currency,
                    now,
                ) {
                    ExchangeRateLookup::Current { rate, expires_at } => {
                        MidPrice::Available { rate, expires_at }
                    }
                    ExchangeRateLookup::Stale => MidPrice::StaleOracleData,
                    ExchangeRateLookup::NoData => MidPrice::NoOracleData,
                }
            });
            advertisement.pricing.quote(mid)
        }
    };
    AdvertisementView {
        advertisement,
        quote,
    }
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getAdvertisement",
        method_fn(
            |state: &NodeState<S>,
             params: IdParams|
             -> Result<Option<AdvertisementView>, RpcError> {
                let now = Timestamp::now();
                let mut cache = HashMap::new();
                Ok(state
                    .advertisements
                    .get(&AdvertisementId::new(params.id))
                    .map(|ad| resolve(state, ad, now, &mut cache)))
            },
        ),
    );
    table.register(
        "getAdvertisements",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<AdvertisementView>, RpcError> {
                // One `now` for the whole response: resolving each row
                // against its own clock read would let a feed lapse
                // partway down the book and return a page that was never
                // true at any single instant.
                let now = Timestamp::now();
                let mut cache = HashMap::new();
                Ok(state
                    .advertisements
                    .all()
                    .into_iter()
                    .map(|ad| resolve(state, ad, now, &mut cache))
                    .collect())
            },
        ),
    );
    table.register(
        "sendAdvertisementCreate",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedAdvertisementCreate =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedAdvertisementCreate always serializes");
                let id = state
                    .advertisements
                    .apply_create(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_CREATED,
                    protocol::OFS_SPEC,
                    Priority::Advertisement,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
    // §18/§21: without this a merchant could publish an ad and never take
    // it down through any client — see `AdvertisementRegistry::apply_disable`.
    table.register(
        "sendAdvertisementDisable",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedAdvertisementDisable =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedAdvertisementDisable always serializes");
                state
                    .advertisements
                    .apply_disable(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_DISABLED,
                    protocol::OFS_SPEC,
                    Priority::Advertisement,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    // §17: the "Price changes" refresh trigger — a merchant repricing an
    // existing ad rather than disabling and recreating it.
    table.register(
        "sendAdvertisementPriceUpdate",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedAdvertisementPriceUpdate =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes = wire::to_bytes(&signed)
                    .expect("SignedAdvertisementPriceUpdate always serializes");
                state
                    .advertisements
                    .apply_pricing_update(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_PRICING_UPDATED,
                    protocol::OFS_SPEC,
                    Priority::Advertisement,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{MethodTable, encode_bytes};
    use openfiat_advertisements::events::{
        AdvertisementCreate, AdvertisementDisable, AdvertisementPriceUpdate,
    };
    use openfiat_advertisements::record::{AdvertisementStatus, Direction, PricingModel};
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{Amount, Timestamp};

    fn table_and_state() -> (MethodTable<MemoryStore>, NodeState<MemoryStore>) {
        let mut table = MethodTable::new();
        register(&mut table);
        (table, NodeState::new_for_test(MemoryStore::new()))
    }

    /// Signed payloads reach a `sendX` method base64-encoded, exactly as a
    /// real wallet-signing client would send them.
    fn params(signed: &impl serde::Serialize) -> serde_json::Value {
        let bytes = json::to_bytes(signed).unwrap();
        serde_json::json!({ "data": encode_bytes(&bytes) })
    }

    /// Publishes a USDC/KES rate straight into this node's oracle index,
    /// via a registered market-data provider (the only publisher §5/§15
    /// accepts), expiring `ttl_millis` after `at`.
    fn publish_rate(
        state: &NodeState<MemoryStore>,
        rate: f64,
        at: Timestamp,
        ttl_millis: u64,
    ) -> Keypair {
        use openfiat_oracles::events::{OraclePublish, SignedOraclePublish};
        use openfiat_oracles::record::{OracleData, OracleId};
        use openfiat_registry::{Registration, SignedRegistration};
        use openfiat_types::{MarketDataService, ServiceId, ServiceType};

        let provider = Keypair::generate();
        let peer = peer_id_from_public_key(&provider.public_key()).unwrap();
        state
            .services
            .apply_registration(SignedRegistration::sign(
                Registration {
                    service_id: ServiceId::new("fx"),
                    service_type: ServiceType::MarketData(MarketDataService::FxOracle),
                    provider: peer.clone(),
                    provider_public_key: provider.public_key(),
                    endpoints: vec![],
                    supported_ofs: vec![7000],
                    region: None,
                    capabilities: vec![],
                    pricing: None,
                    payout_wallet: None,
                    timestamp: at,
                },
                &provider,
            ))
            .expect("a market-data registration must be accepted");

        state
            .oracles
            .apply_publish(SignedOraclePublish::sign(
                OraclePublish {
                    id: OracleId::new("usdc-kes"),
                    provider: peer,
                    provider_public_key: provider.public_key(),
                    data: OracleData::ExchangeRate {
                        base: "USDC".to_string(),
                        quote: "KES".to_string(),
                        rate,
                    },
                    version: 1,
                    timestamp: at,
                    expires_at: Timestamp::from_millis(at.as_millis() + ttl_millis),
                },
                &provider,
            ))
            .expect("a registered provider's publish must be accepted");
        provider
    }

    /// A floating USDC/KES advertisement at `premium_bps` over mid.
    fn floating_create(keypair: &Keypair, id: &str, premium_bps: i32) -> AdvertisementCreate {
        AdvertisementCreate {
            fiat_currency: "KES".to_string(),
            pricing: PricingModel::Floating {
                oracle_provider: "any".to_string(),
                premium_bps,
                price_decimals: 2,
            },
            ..create_at(keypair, id, Timestamp::from_millis(1_000))
        }
    }

    fn quote_of(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        id: &str,
    ) -> serde_json::Value {
        let result = table
            .dispatch(state, "getAdvertisement", serde_json::json!({ "id": id }))
            .expect("the advertisement must be readable");
        result["quote"].clone()
    }

    fn create_at(keypair: &Keypair, id: &str, at: Timestamp) -> AdvertisementCreate {
        AdvertisementCreate {
            id: AdvertisementId::new(id),
            merchant: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            merchant_public_key: keypair.public_key(),
            asset: "USDC".to_string(),
            direction: Direction::Sell,
            fiat_currency: "USD".to_string(),
            min_trade: Amount::new(1_000, 2),
            max_trade: Amount::new(100_000, 2),
            initial_liquidity: Amount::new(1_000_000, 2),
            pricing: PricingModel::Fixed {
                price: Amount::new(100, 2),
            },
            payment_methods: vec!["bank_transfer".to_string()],
            timestamp: at,
        }
    }

    fn create_advertisement(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        keypair: &Keypair,
        id: &str,
        at: Timestamp,
    ) -> AdvertisementId {
        let signed = SignedAdvertisementCreate::sign(create_at(keypair, id, at), keypair);
        table
            .dispatch(state, "sendAdvertisementCreate", params(&signed))
            .expect("creation must be accepted");
        AdvertisementId::new(id)
    }

    /// The interlock this whole change exists to unlock: before
    /// `sendAdvertisementDisable` was registered, nothing could reach
    /// `AdvertisementRegistry::apply_disable` — a merchant could publish an
    /// ad and never take it down through any client. Proves the method is
    /// actually reachable through the table, not just present in the crate.
    #[test]
    fn the_owning_merchant_can_disable_their_own_advertisement() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let id = create_advertisement(
            &table,
            &state,
            &owner,
            "ad-1",
            Timestamp::from_millis(1_000),
        );

        let signed = SignedAdvertisementDisable::sign(
            AdvertisementDisable {
                id: id.clone(),
                merchant: peer_id_from_public_key(&owner.public_key()).unwrap(),
                timestamp: Timestamp::from_millis(2_000),
            },
            &owner,
        );
        table
            .dispatch(&state, "sendAdvertisementDisable", params(&signed))
            .expect("the owner's own disable must be accepted");

        assert_eq!(
            state.advertisements.get(&id).unwrap().status,
            AdvertisementStatus::Disabled
        );
    }

    #[test]
    fn a_disable_signed_by_an_impostor_is_rejected() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let id = create_advertisement(
            &table,
            &state,
            &owner,
            "ad-1",
            Timestamp::from_millis(1_000),
        );

        // The attacker names the real merchant but signs with its own key.
        let attacker = Keypair::generate();
        let forged = SignedAdvertisementDisable::sign(
            AdvertisementDisable {
                id: id.clone(),
                merchant: peer_id_from_public_key(&owner.public_key()).unwrap(),
                timestamp: Timestamp::from_millis(2_000),
            },
            &attacker,
        );

        assert!(
            table
                .dispatch(&state, "sendAdvertisementDisable", params(&forged))
                .is_err(),
            "a disable must be verified against the key already on file"
        );
        assert_eq!(
            state.advertisements.get(&id).unwrap().status,
            AdvertisementStatus::Active,
            "a rejected disable must not change the advertisement's status"
        );
    }

    /// Same interlock as the disable test above, for the other half of the
    /// bug: `sendAdvertisementPriceUpdate` reaching
    /// `AdvertisementRegistry::apply_pricing_update`.
    #[test]
    fn the_owning_merchant_can_reprice_their_own_advertisement() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let id = create_advertisement(
            &table,
            &state,
            &owner,
            "ad-1",
            Timestamp::from_millis(1_000),
        );

        let new_price = PricingModel::Fixed {
            price: Amount::new(200, 2),
        };
        let signed = SignedAdvertisementPriceUpdate::sign(
            AdvertisementPriceUpdate {
                id: id.clone(),
                merchant: peer_id_from_public_key(&owner.public_key()).unwrap(),
                pricing: new_price.clone(),
                timestamp: Timestamp::from_millis(2_000),
            },
            &owner,
        );
        table
            .dispatch(&state, "sendAdvertisementPriceUpdate", params(&signed))
            .expect("the owner's own price update must be accepted");

        assert_eq!(state.advertisements.get(&id).unwrap().pricing, new_price);
    }

    #[test]
    fn a_price_update_signed_by_an_impostor_is_rejected() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let id = create_advertisement(
            &table,
            &state,
            &owner,
            "ad-1",
            Timestamp::from_millis(1_000),
        );
        let original_pricing = state.advertisements.get(&id).unwrap().pricing;

        let attacker = Keypair::generate();
        let forged = SignedAdvertisementPriceUpdate::sign(
            AdvertisementPriceUpdate {
                id: id.clone(),
                merchant: peer_id_from_public_key(&owner.public_key()).unwrap(),
                pricing: PricingModel::Fixed {
                    price: Amount::new(99_900, 2),
                },
                timestamp: Timestamp::from_millis(2_000),
            },
            &attacker,
        );

        assert!(
            table
                .dispatch(&state, "sendAdvertisementPriceUpdate", params(&forged))
                .is_err(),
            "a price update must be verified against the key already on file"
        );
        assert_eq!(
            state.advertisements.get(&id).unwrap().pricing,
            original_pricing,
            "a rejected price update must not change the stored pricing"
        );
    }

    /// The join this change exists to make: a floating advertisement plus a
    /// live oracle record comes back through RPC as an actual number.
    /// Before this, `PricingModel::Floating` was configuration nothing ever
    /// turned into a price.
    #[test]
    fn a_floating_advertisement_resolves_against_a_live_oracle_record() {
        let (table, state) = table_and_state();
        let merchant = Keypair::generate();
        publish_rate(&state, 129.52, Timestamp::now(), 600_000);
        let signed =
            SignedAdvertisementCreate::sign(floating_create(&merchant, "ad-float", 150), &merchant);
        table
            .dispatch(&state, "sendAdvertisementCreate", params(&signed))
            .expect("creation must be accepted");

        let quote = quote_of(&table, &state, "ad-float");
        assert_eq!(quote["kind"], "Floating");
        // 129.52 * 1.015 = 131.4628, to the cent.
        assert_eq!(quote["price"]["base_units"], 13_146);
        assert_eq!(quote["price"]["decimals"], 2);
        assert_eq!(quote["mid_rate"], 129.52);
        assert_eq!(quote["premium_bps"], 150);
    }

    /// The failure that silently sells at yesterday's rate. An oracle
    /// record whose `expires_at` has passed must leave the advertisement
    /// with no price at all — not the lapsed rate, not a fallback.
    #[test]
    fn an_expired_oracle_record_does_not_price_an_advertisement() {
        let (table, state) = table_and_state();
        let merchant = Keypair::generate();
        // Published far enough in the past that its TTL has already run out
        // by the time the RPC resolves it against `Timestamp::now()`.
        let long_ago = Timestamp::from_millis(Timestamp::now().as_millis() - 600_000);
        publish_rate(&state, 129.52, long_ago, 1_000);
        let signed =
            SignedAdvertisementCreate::sign(floating_create(&merchant, "ad-stale", 150), &merchant);
        table
            .dispatch(&state, "sendAdvertisementCreate", params(&signed))
            .expect("creation must be accepted");

        let quote = quote_of(&table, &state, "ad-stale");
        assert_eq!(quote["kind"], "Unpriceable");
        assert_eq!(quote["reason"], "StaleOracleData");
        assert!(
            quote.get("price").is_none(),
            "a lapsed record must not yield a price field at all, got {quote}"
        );
    }

    /// The other unpriceable case: nothing publishes this pair. Must be
    /// expressible as "no price" rather than defaulting to a number.
    #[test]
    fn an_advertisement_with_no_oracle_data_is_unpriceable_not_priced() {
        let (table, state) = table_and_state();
        let merchant = Keypair::generate();
        // No `publish_rate` at all — the oracle index is empty.
        let signed = SignedAdvertisementCreate::sign(
            floating_create(&merchant, "ad-nodata", 150),
            &merchant,
        );
        table
            .dispatch(&state, "sendAdvertisementCreate", params(&signed))
            .expect("creation must be accepted");

        let quote = quote_of(&table, &state, "ad-nodata");
        assert_eq!(quote["kind"], "Unpriceable");
        assert_eq!(quote["reason"], "NoOracleData");
        assert!(quote.get("price").is_none());
    }

    /// A dead feed must not take fixed-price advertisements down with it —
    /// they never consult the oracle.
    #[test]
    fn a_fixed_advertisement_still_prices_when_the_oracle_is_empty() {
        let (table, state) = table_and_state();
        let merchant = Keypair::generate();
        create_advertisement(
            &table,
            &state,
            &merchant,
            "ad-fixed",
            Timestamp::from_millis(1_000),
        );

        let quote = quote_of(&table, &state, "ad-fixed");
        assert_eq!(quote["kind"], "Fixed");
        assert_eq!(quote["price"]["base_units"], 100);
    }

    /// The view is additive: every field a client already read off an
    /// advertisement is still at the top level, unmoved.
    #[test]
    fn the_advertisement_record_is_unchanged_alongside_its_quote() {
        let (table, state) = table_and_state();
        let merchant = Keypair::generate();
        create_advertisement(
            &table,
            &state,
            &merchant,
            "ad-1",
            Timestamp::from_millis(1_000),
        );

        let result = table
            .dispatch(
                &state,
                "getAdvertisement",
                serde_json::json!({ "id": "ad-1" }),
            )
            .unwrap();
        assert_eq!(result["id"], "ad-1");
        assert_eq!(result["asset"], "USDC");
        assert_eq!(result["status"], "Active");
        assert!(result["pricing"]["Fixed"].is_object());
        assert!(result["quote"].is_object());
    }

    /// Every row of one response resolves off a single oracle read, so a
    /// page can never mix rows priced before and after a feed lapsed.
    #[test]
    fn a_listing_prices_every_floating_row_off_the_same_read() {
        let (table, state) = table_and_state();
        publish_rate(&state, 129.52, Timestamp::now(), 600_000);
        for (i, premium) in [0, 150].iter().enumerate() {
            let merchant = Keypair::generate();
            let signed = SignedAdvertisementCreate::sign(
                floating_create(&merchant, &format!("ad-{i}"), *premium),
                &merchant,
            );
            table
                .dispatch(&state, "sendAdvertisementCreate", params(&signed))
                .expect("creation must be accepted");
        }

        let result = table
            .dispatch(&state, "getAdvertisements", serde_json::json!({}))
            .unwrap();
        let rows = result.as_array().expect("a listing is an array");
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(row["quote"]["kind"], "Floating");
            assert_eq!(row["quote"]["mid_rate"], 129.52);
        }
    }
}
