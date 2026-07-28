//! Drives one node's risk index: applies incoming gossip events
//! automatically and provides the operation that originates new ones.
//! Provider registration reuses `openfiat-registry` directly — this
//! crate has no registration event of its own.
//!
//! Publishing a `Flagged` record requires the underlying `GossipService`
//! to have been constructed with `NodeRole::RiskIntelligenceProvider`
//! among its roles — `WalletFlagged` is one of the role-scoped event
//! types `openfiat-gossip`'s own origination authorization (OGP §7)
//! restricts; `WalletCleared` (used for `Cleared` records) isn't
//! currently role-restricted there.

use crate::error::RiskError;
use crate::events::{RiskPublish, SignedRiskPublish};
use crate::protocol;
use crate::record::{Confidence, ProviderCategory, RiskOutcome, RiskRecord, RiskRecordId, ScreeningResult, Severity};
use crate::store::RiskIndex;
use openfiat_gossip::GossipService;
use openfiat_registry::Registry;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, PeerId, Priority, Timestamp};
use std::rc::Rc;

pub struct RiskService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<RiskIndex<S>>,
}

impl<S: KvStore + 'static> RiskService<S> {
    /// `services` is the shared handle from `RegistryService::registry`
    /// on the same node — see `RiskIndex`.
    pub fn new(mut gossip: GossipService<S>, store: S, services: Rc<Registry<S>>) -> Self {
        let registry = Rc::new(RiskIndex::new(store, services));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn get(&self, id: &RiskRecordId) -> Option<RiskRecord> {
        self.registry.get(id)
    }

    pub fn for_wallet(&self, wallet: &PeerId) -> Vec<RiskRecord> {
        self.registry.for_wallet(wallet)
    }

    pub fn screen(&self, wallet: &PeerId) -> ScreeningResult {
        self.registry.screen(wallet, Timestamp::now())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &mut self,
        id: impl Into<String>,
        wallet: PeerId,
        category: ProviderCategory,
        outcome: RiskOutcome,
        severity: Severity,
        confidence: Confidence,
        reason: impl Into<String>,
        evidence: Vec<String>,
        expires_at: Option<Timestamp>,
    ) -> Result<RiskRecordId, RiskError> {
        let publish = RiskPublish {
            id: RiskRecordId::new(id),
            provider: self.gossip.node.local_peer_id(),
            provider_public_key: self.gossip.public_key(),
            wallet,
            category,
            outcome,
            severity,
            confidence,
            reason: reason.into(),
            evidence,
            timestamp: Timestamp::now(),
            expires_at,
        };
        let bytes = wire::to_bytes(&publish).map_err(|_| RiskError::MalformedRecord)?;
        let signed = SignedRiskPublish { signature: self.gossip.sign(&bytes), publish };
        let event_type = match outcome {
            RiskOutcome::Flagged => protocol::EVENT_FLAGGED,
            RiskOutcome::Cleared => protocol::EVENT_CLEARED,
        };
        self.originate(event_type, &signed)?;
        Ok(signed.publish.id)
    }

    fn originate(&mut self, event_type: &str, payload: &impl serde::Serialize) -> Result<(), RiskError> {
        let bytes = wire::to_bytes(payload).map_err(|_| RiskError::MalformedRecord)?;
        let event_type = EventType::new(event_type).expect("risk event names are all valid PascalCase identifiers");
        self.gossip
            .originate(event_type, protocol::OFS_SPEC, Priority::Reputation, 8, bytes)
            .map(|_| ())
            .map_err(|_| RiskError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
