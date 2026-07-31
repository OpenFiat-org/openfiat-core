//! Drives one node's trade-channel index: applies incoming gossip events
//! automatically, and provides the two operations that originate new
//! ones.
//!
//! # Where the key lives
//!
//! Nowhere in this type. A `ChannelKey` is passed in at each call and
//! dropped when the call returns; the service never stores one, and this
//! module has no field that could hold one. That is deliberate: a node
//! process that retained channel keys would be a node process whose
//! operator can read every trade it relays, whatever the rest of this
//! crate says.
//!
//! In the shipped product the key never reaches a node at all — a wallet
//! generates it, seals it, encrypts against it, and submits already-signed
//! events through `sendTradeChannelKeyGrant`/`sendTradeChannelEntry`, the
//! same way it submits every other signed payload. This service exists
//! for the replication test, and for anyone embedding a node and a wallet
//! in one process, which is what makes the key an argument rather than
//! state.

use crate::error::TradeChannelError;
use crate::events::{
    SignedTradeChannelEntryPost, SignedTradeChannelKeyGrant, TradeChannelEntryPost,
    TradeChannelKeyGrant,
};
use crate::key::{ChannelKey, EntryBinding, seal_entry};
use crate::protocol;
use crate::record::{EntryKind, TradeChannel};
use crate::store::TradeChannelRegistry;
use openfiat_crypto::seal;
use openfiat_disputes::DisputeRegistry;
use openfiat_gossip::GossipService;
use openfiat_serialization::{json, wire};
use openfiat_settlement::{SettlementId, SettlementRegistry};
use openfiat_storage::KvStore;
use openfiat_types::{EventType, PeerId, Priority, PublicKey, Timestamp};
use std::rc::Rc;

pub struct TradeChannelService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<TradeChannelRegistry<S>>,
}

impl<S: KvStore + 'static> TradeChannelService<S> {
    pub fn new(
        mut gossip: GossipService<S>,
        store: S,
        settlements: Rc<SettlementRegistry<S>>,
        disputes: Rc<DisputeRegistry<S>>,
    ) -> Self {
        let registry = Rc::new(TradeChannelRegistry::new(store, settlements, disputes));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn registry(&self) -> Rc<TradeChannelRegistry<S>> {
        Rc::clone(&self.registry)
    }

    pub fn channel(&self, settlement_id: &SettlementId) -> TradeChannel {
        self.registry.channel(settlement_id)
    }

    /// Seal `key` to `recipient` and publish the grant.
    ///
    /// `recipient_public_key` is what the key is sealed to, and the
    /// caller must have obtained it from a record that binds it to the
    /// recipient's peer id — the settlement for a party, the dispute's
    /// `arbitrator_keys` for an arbitrator. Sealing to a key someone
    /// merely claimed is how a channel gets handed to an impostor.
    pub fn grant_key(
        &mut self,
        settlement_id: SettlementId,
        recipient: PeerId,
        recipient_public_key: &PublicKey,
        key: &ChannelKey,
    ) -> Result<(), TradeChannelError> {
        let grant = TradeChannelKeyGrant {
            settlement_id,
            granter: self.gossip.node.local_peer_id(),
            recipient,
            key_id: key.id(),
            sealed_key: seal(recipient_public_key, key.expose())
                .map_err(|_| TradeChannelError::RecipientNotPermitted)?,
            timestamp: Timestamp::now(),
        };
        let bytes = json::to_bytes(&grant).map_err(|_| TradeChannelError::MalformedEntry)?;
        let signed = SignedTradeChannelKeyGrant {
            signature: self.gossip.sign(&bytes),
            grant,
        };
        self.registry.apply_key_grant(signed.clone())?;
        self.originate(protocol::EVENT_KEY_GRANTED, &signed)
    }

    /// Encrypt `plaintext` under `key` and publish it as this node's next
    /// entry in the channel.
    pub fn post(
        &mut self,
        settlement_id: SettlementId,
        kind: EntryKind,
        key: &ChannelKey,
        plaintext: &[u8],
    ) -> Result<u64, TradeChannelError> {
        let author = self.gossip.node.local_peer_id();
        let sequence = self.registry.channel(&settlement_id).next_sequence(&author);
        let payload = seal_entry(
            key,
            &EntryBinding {
                settlement_id: &settlement_id,
                author: &author,
                sequence,
                kind: kind.name(),
            },
            plaintext,
        )?;
        let post = TradeChannelEntryPost {
            settlement_id,
            author,
            sequence,
            kind,
            payload,
            timestamp: Timestamp::now(),
        };
        let bytes = json::to_bytes(&post).map_err(|_| TradeChannelError::MalformedEntry)?;
        let signed = SignedTradeChannelEntryPost {
            signature: self.gossip.sign(&bytes),
            post,
        };
        self.registry.apply_entry(signed.clone())?;
        self.originate(protocol::EVENT_ENTRY_POSTED, &signed)?;
        Ok(sequence)
    }

    fn originate(
        &mut self,
        event_type: &str,
        payload: &impl serde::Serialize,
    ) -> Result<(), TradeChannelError> {
        let bytes = wire::to_bytes(payload).map_err(|_| TradeChannelError::MalformedEntry)?;
        let event_type = EventType::new(event_type)
            .expect("trade-channel event names are valid PascalCase identifiers");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                // Same class as the settlement these entries hang off:
                // payment details the buyer is waiting on are as
                // time-critical as the settlement events around them, and
                // a chat message that arrives after the trade times out
                // is worse than useless.
                Priority::SessionReservationSettlement,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| TradeChannelError::NotAParty)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
