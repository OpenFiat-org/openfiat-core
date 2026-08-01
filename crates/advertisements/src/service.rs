//! Drives one node's advertisement index: applies incoming gossip events
//! automatically (via `GossipService`'s event hook) and provides the
//! create/status/terms/pricing operations that originate new ones.

use crate::error::AdvertisementError;
use crate::events::{
    AdvertisementCreate, AdvertisementPriceUpdate, AdvertisementStatusSet,
    AdvertisementTermsUpdate, SignedAdvertisementCreate, SignedAdvertisementPriceUpdate,
    SignedAdvertisementStatusSet, SignedAdvertisementTermsUpdate,
};
use crate::protocol;
use crate::record::{Advertisement, AdvertisementId, AdvertisementStatus, Direction, PricingModel};
use crate::store::AdvertisementRegistry;
use openfiat_crypto::MintAddress;
use openfiat_gossip::GossipService;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_taxonomy::PaymentMethodRef;
use openfiat_types::{Amount, EventType, FiatCurrency, Priority, Timestamp};
use std::rc::Rc;

pub struct AdvertisementService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<AdvertisementRegistry<S>>,
}

impl<S: KvStore + 'static> AdvertisementService<S> {
    pub fn new(mut gossip: GossipService<S>, store: S) -> Self {
        let registry = Rc::new(AdvertisementRegistry::new(store));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    /// A shared handle to this node's advertisement index, for crates
    /// downstream in the dependency chain (reservations, settlement) that
    /// need to validate against and adjust liquidity on the same replica
    /// this service maintains — see `openfiat-reservations` for the first
    /// consumer.
    pub fn registry(&self) -> Rc<AdvertisementRegistry<S>> {
        Rc::clone(&self.registry)
    }

    pub fn get(&self, id: &AdvertisementId) -> Option<Advertisement> {
        self.registry.get(id)
    }

    pub fn find_active(&self, direction: Direction) -> Vec<Advertisement> {
        self.registry.find_active(direction)
    }

    /// §9-10: apply a reservation's liquidity lock directly to the local
    /// index. Real wiring is "consume a reservation-opened gossip event
    /// once `openfiat-reservations` exists"; exposed directly for now so
    /// this crate's liquidity bookkeeping is usable and testable today.
    pub fn reserve_liquidity(
        &self,
        id: &AdvertisementId,
        amount: Amount,
    ) -> Result<(), AdvertisementError> {
        self.registry.reserve_liquidity(id, amount)
    }

    pub fn release_liquidity(
        &self,
        id: &AdvertisementId,
        amount: Amount,
    ) -> Result<(), AdvertisementError> {
        self.registry.release_liquidity(id, amount)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &mut self,
        id: impl Into<String>,
        asset_mint: MintAddress,
        direction: Direction,
        fiat_currency: FiatCurrency,
        min_trade: Amount,
        max_trade: Amount,
        initial_liquidity: Amount,
        pricing: PricingModel,
        payment_methods: Vec<PaymentMethodRef>,
    ) -> Result<AdvertisementId, AdvertisementError> {
        let create = AdvertisementCreate {
            id: AdvertisementId::new(id),
            merchant: self.gossip.node.local_peer_id(),
            merchant_public_key: self.gossip.public_key(),
            asset_mint,
            direction,
            fiat_currency,
            min_trade,
            max_trade,
            initial_liquidity,
            pricing,
            payment_methods,
            timestamp: Timestamp::now(),
        };
        let bytes = json::to_bytes(&create).expect("AdvertisementCreate always serializes");
        let signed = SignedAdvertisementCreate {
            signature: self.gossip.sign(&bytes),
            create,
        };
        self.originate(protocol::EVENT_CREATED, &signed)?;
        Ok(signed.create.id)
    }

    /// Pauses, disables, deletes, or reactivates an advertisement.
    ///
    /// `set_status(id, Disabled)` is what `disable` used to be. The
    /// other three states were unreachable.
    pub fn set_status(
        &mut self,
        id: AdvertisementId,
        status: AdvertisementStatus,
    ) -> Result<(), AdvertisementError> {
        let set = AdvertisementStatusSet {
            id,
            merchant: self.gossip.node.local_peer_id(),
            status,
            timestamp: Timestamp::now(),
        };
        let bytes = json::to_bytes(&set).map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        let signed = SignedAdvertisementStatusSet {
            signature: self.gossip.sign(&bytes),
            set,
        };
        self.originate(protocol::EVENT_STATUS_SET, &signed)
    }

    /// Changes trade limits and payment methods, keeping the id.
    pub fn update_terms(
        &mut self,
        id: AdvertisementId,
        min_trade: Amount,
        max_trade: Amount,
        payment_methods: Vec<PaymentMethodRef>,
    ) -> Result<(), AdvertisementError> {
        let update = AdvertisementTermsUpdate {
            id,
            merchant: self.gossip.node.local_peer_id(),
            min_trade,
            max_trade,
            payment_methods,
            timestamp: Timestamp::now(),
        };
        let bytes =
            json::to_bytes(&update).map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        let signed = SignedAdvertisementTermsUpdate {
            signature: self.gossip.sign(&bytes),
            update,
        };
        self.originate(protocol::EVENT_TERMS_UPDATED, &signed)
    }

    pub fn update_pricing(
        &mut self,
        id: AdvertisementId,
        pricing: PricingModel,
    ) -> Result<(), AdvertisementError> {
        let update = AdvertisementPriceUpdate {
            id,
            merchant: self.gossip.node.local_peer_id(),
            pricing,
            timestamp: Timestamp::now(),
        };
        let bytes =
            json::to_bytes(&update).map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        let signed = SignedAdvertisementPriceUpdate {
            signature: self.gossip.sign(&bytes),
            update,
        };
        self.originate(protocol::EVENT_PRICING_UPDATED, &signed)
    }

    fn originate(
        &mut self,
        event_type: &str,
        payload: &impl serde::Serialize,
    ) -> Result<(), AdvertisementError> {
        let bytes =
            wire::to_bytes(payload).map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        let event_type = EventType::new(event_type)
            .expect("advertisement event names are all valid PascalCase identifiers");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::Advertisement,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| AdvertisementError::UnauthorizedUpdate)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
