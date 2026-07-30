//! Advertisement methods (OFS-2100).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_advertisements::events::{
    SignedAdvertisementCreate, SignedAdvertisementDisable, SignedAdvertisementPriceUpdate,
};
use openfiat_advertisements::protocol;
use openfiat_advertisements::{Advertisement, AdvertisementId};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getAdvertisement",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<Advertisement>, RpcError> {
                Ok(state.advertisements.get(&AdvertisementId::new(params.id)))
            },
        ),
    );
    table.register(
        "getAdvertisements",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<Advertisement>, RpcError> { Ok(state.advertisements.all()) },
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
}
