//! The replicated local advertisement index. Same shape as
//! `openfiat-registry`'s store: generic over `KvStore`, populated purely
//! by consuming gossip events (§23).

use crate::error::AdvertisementError;
use crate::events::{
    SignedAdvertisementCreate, SignedAdvertisementDisable, SignedAdvertisementPriceUpdate,
};
use crate::protocol;
use crate::record::{Advertisement, AdvertisementId, AdvertisementStatus, Direction};
use openfiat_crypto::verify;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{Amount, EventEnvelope, Timestamp};

const COLUMN_FAMILY: &str = "advertisements";

pub struct AdvertisementRegistry<S> {
    store: S,
}

impl<S: KvStore> AdvertisementRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get(&self, id: &AdvertisementId) -> Option<Advertisement> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, ad: &Advertisement) {
        if let Ok(bytes) = wire::to_bytes(ad) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, ad.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Advertisement> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// Only advertisements currently visible for trading (§8, §16, §18 —
    /// active, not vacationing, not disabled or deleted).
    pub fn find_active(&self, direction: Direction) -> Vec<Advertisement> {
        self.all()
            .into_iter()
            .filter(|ad| ad.direction == direction && ad.status == AdvertisementStatus::Active)
            .collect()
    }

    pub fn apply_create(
        &self,
        signed: SignedAdvertisementCreate,
    ) -> Result<AdvertisementId, AdvertisementError> {
        signed.verify()?;
        let id = signed.create.id.clone();
        if self.get(&id).is_some() {
            return Err(AdvertisementError::DuplicateAdvertisementId);
        }
        let create = signed.create;
        self.put(&Advertisement {
            id: create.id,
            merchant: create.merchant,
            merchant_public_key: create.merchant_public_key,
            asset_mint: create.asset_mint,
            direction: create.direction,
            fiat_currency: create.fiat_currency,
            min_trade: create.min_trade,
            max_trade: create.max_trade,
            available_liquidity: create.initial_liquidity,
            pricing: create.pricing,
            payment_methods: create.payment_methods,
            status: AdvertisementStatus::Active,
            created_at: create.timestamp,
            updated_at: create.timestamp,
        });
        Ok(id)
    }

    /// §21/§24: only the original merchant may disable their own ad.
    pub fn apply_disable(
        &self,
        signed: SignedAdvertisementDisable,
    ) -> Result<(), AdvertisementError> {
        let mut ad = self
            .get(&signed.disable.id)
            .ok_or(AdvertisementError::AdvertisementNotFound)?;
        if ad.merchant != signed.disable.merchant {
            return Err(AdvertisementError::UnauthorizedUpdate);
        }
        let bytes = json::to_bytes(&signed.disable)
            .map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        verify(&ad.merchant_public_key, &bytes, &signed.signature)
            .map_err(|_| AdvertisementError::InvalidSignature)?;

        ad.status = AdvertisementStatus::Disabled;
        ad.updated_at = signed.disable.timestamp;
        self.put(&ad);
        Ok(())
    }

    pub fn apply_pricing_update(
        &self,
        signed: SignedAdvertisementPriceUpdate,
    ) -> Result<(), AdvertisementError> {
        let mut ad = self
            .get(&signed.update.id)
            .ok_or(AdvertisementError::AdvertisementNotFound)?;
        if ad.merchant != signed.update.merchant {
            return Err(AdvertisementError::UnauthorizedUpdate);
        }
        let bytes = json::to_bytes(&signed.update)
            .map_err(|_| AdvertisementError::MalformedAdvertisement)?;
        verify(&ad.merchant_public_key, &bytes, &signed.signature)
            .map_err(|_| AdvertisementError::InvalidSignature)?;

        ad.pricing = signed.update.pricing;
        ad.updated_at = signed.update.timestamp;
        self.put(&ad);
        Ok(())
    }

    /// §9-10: lock `amount` of an advertisement's available liquidity —
    /// the effect a reservation has on the ad it was opened against. Not
    /// a signed merchant action; driven by protocol events from whichever
    /// crate manages reservations. Automatically disables the ad if this
    /// exhausts its liquidity (§18).
    pub fn reserve_liquidity(
        &self,
        id: &AdvertisementId,
        amount: Amount,
    ) -> Result<(), AdvertisementError> {
        let mut ad = self
            .get(id)
            .ok_or(AdvertisementError::AdvertisementNotFound)?;
        let remaining = ad
            .available_liquidity
            .checked_sub(amount)
            .ok_or(AdvertisementError::InsufficientLiquidity)?;
        ad.available_liquidity = remaining;
        if remaining.base_units() == 0 {
            ad.status = AdvertisementStatus::Disabled;
        }
        ad.updated_at = Timestamp::now();
        self.put(&ad);
        Ok(())
    }

    /// The inverse of [`Self::reserve_liquidity`] — a cancelled or
    /// expired reservation returns its amount to the pool (§10).
    pub fn release_liquidity(
        &self,
        id: &AdvertisementId,
        amount: Amount,
    ) -> Result<(), AdvertisementError> {
        let mut ad = self
            .get(id)
            .ok_or(AdvertisementError::AdvertisementNotFound)?;
        ad.available_liquidity = ad
            .available_liquidity
            .checked_add(amount)
            .ok_or(AdvertisementError::MalformedAdvertisement)?;
        ad.updated_at = Timestamp::now();
        self.put(&ad);
        Ok(())
    }

    /// Apply a gossip event to this index, if it's one of ours.
    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match event.event_type.as_str() {
            protocol::EVENT_CREATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_create(signed);
                }
            }
            protocol::EVENT_DISABLED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_disable(signed);
                }
            }
            protocol::EVENT_PRICING_UPDATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_pricing_update(signed);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AdvertisementCreate;
    use crate::record::PricingModel;
    use openfiat_crypto::Keypair;
    use openfiat_crypto::MintAddress;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::FiatCurrency;

    fn create(keypair: &Keypair, id: &str, liquidity: u64) -> AdvertisementCreate {
        AdvertisementCreate {
            id: AdvertisementId::new(id),
            merchant: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            merchant_public_key: keypair.public_key(),
            asset_mint: MintAddress::parse("2bHPi5hA4zrmPAfrvLmEexg3KJjpTjNkUcxWnzUPeRRU").unwrap(),
            direction: Direction::Sell,
            fiat_currency: FiatCurrency::parse("KES").unwrap(),
            min_trade: Amount::new(10_000_000, 6),
            max_trade: Amount::new(10_000_000_000, 6),
            initial_liquidity: Amount::new(liquidity, 6),
            pricing: PricingModel::Fixed {
                price: Amount::new(129_000_000, 6),
            },
            payment_methods: vec!["Mobile Money".to_string()],
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn a_created_advertisement_is_active_and_queryable() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&keypair, "ad-1", 10_000_000_000),
                &keypair,
            ))
            .unwrap();
        let ad = registry.get(&id).unwrap();
        assert_eq!(ad.status, AdvertisementStatus::Active);
        assert_eq!(registry.find_active(Direction::Sell), vec![ad]);
    }

    #[test]
    fn reserving_more_than_available_liquidity_is_rejected() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&keypair, "ad-1", 1_000_000),
                &keypair,
            ))
            .unwrap();

        let result = registry.reserve_liquidity(&id, Amount::new(2_000_000, 6));
        assert_eq!(result, Err(AdvertisementError::InsufficientLiquidity));
        assert_eq!(
            registry.get(&id).unwrap().available_liquidity,
            Amount::new(1_000_000, 6)
        );
    }

    #[test]
    fn exhausting_liquidity_automatically_disables_the_advertisement() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&keypair, "ad-1", 1_000_000),
                &keypair,
            ))
            .unwrap();

        registry
            .reserve_liquidity(&id, Amount::new(1_000_000, 6))
            .unwrap();
        let ad = registry.get(&id).unwrap();
        assert_eq!(ad.available_liquidity, Amount::new(0, 6));
        assert_eq!(ad.status, AdvertisementStatus::Disabled);
        assert!(registry.find_active(Direction::Sell).is_empty());
    }

    #[test]
    fn releasing_liquidity_restores_it() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let keypair = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&keypair, "ad-1", 5_000_000),
                &keypair,
            ))
            .unwrap();

        registry
            .reserve_liquidity(&id, Amount::new(2_000_000, 6))
            .unwrap();
        registry
            .release_liquidity(&id, Amount::new(2_000_000, 6))
            .unwrap();
        assert_eq!(
            registry.get(&id).unwrap().available_liquidity,
            Amount::new(5_000_000, 6)
        );
    }

    #[test]
    fn disabling_from_a_different_merchant_is_rejected() {
        let registry = AdvertisementRegistry::new(MemoryStore::new());
        let owner = Keypair::generate();
        let id = registry
            .apply_create(SignedAdvertisementCreate::sign(
                create(&owner, "ad-1", 1_000_000),
                &owner,
            ))
            .unwrap();

        let attacker = Keypair::generate();
        let signed = crate::events::SignedAdvertisementDisable::sign(
            crate::events::AdvertisementDisable {
                id,
                merchant: peer_id_from_public_key(&owner.public_key()).unwrap(),
                timestamp: Timestamp::now(),
            },
            &attacker,
        );
        let result = registry.apply_disable(signed);
        assert_eq!(result, Err(AdvertisementError::InvalidSignature));
    }
}
