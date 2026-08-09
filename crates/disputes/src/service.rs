//! Drives one node's dispute index: applies incoming gossip events
//! automatically and provides the operations that originate new ones.

use crate::commitment;
use crate::error::DisputeError;
use crate::events::{
    ArbitratorJoin, DisputeOpen, MutualSettlementAgree, SignedArbitratorJoin, SignedDisputeOpen,
    SignedMutualSettlementAgree, SignedVoteCommit, SignedVoteReveal, VoteCommit, VoteReveal,
};
use crate::protocol;
use crate::record::{Dispute, DisputeId, Vote};
use crate::store::DisputeRegistry;
use openfiat_gossip::GossipService;
use openfiat_serialization::wire;
use openfiat_settlement::{SettlementId, SettlementRegistry};
use openfiat_storage::KvStore;
use openfiat_types::{EventType, Priority, Timestamp};
use std::rc::Rc;

pub struct DisputeService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<DisputeRegistry<S>>,
}

impl<S: KvStore + 'static> DisputeService<S> {
    /// `settlements` is the shared handle from `SettlementService::registry`
    /// on the same node — a dispute's buyer/seller are read from there.
    pub fn new(
        mut gossip: GossipService<S>,
        store: S,
        settlements: Rc<SettlementRegistry<S>>,
    ) -> Self {
        let registry = Rc::new(DisputeRegistry::new(store, settlements));
        let handler_registry = Rc::clone(&registry);
        gossip.add_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn get(&self, id: &DisputeId) -> Option<Dispute> {
        self.registry.get(id)
    }

    pub fn all(&self) -> Vec<Dispute> {
        self.registry.all()
    }

    pub fn open(
        &mut self,
        id: impl Into<String>,
        settlement_id: SettlementId,
        reason: impl Into<String>,
    ) -> Result<DisputeId, DisputeError> {
        let open = DisputeOpen {
            id: DisputeId::new(id),
            settlement_id,
            opener: self.gossip.node.local_peer_id(),
            opener_public_key: self.gossip.public_key(),
            reason: reason.into(),
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::DISPUTE_OPEN,
            &open,
        )
        .expect("DisputeOpen always serializes");
        let signed = SignedDisputeOpen {
            signature: self.gossip.sign(&bytes),
            open,
        };
        self.originate(protocol::EVENT_OPENED, &signed)?;
        Ok(signed.open.id)
    }

    pub fn join_as_arbitrator(&mut self, dispute_id: DisputeId) -> Result<(), DisputeError> {
        let join = ArbitratorJoin {
            dispute_id,
            arbitrator: self.gossip.node.local_peer_id(),
            arbitrator_public_key: self.gossip.public_key(),
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::ARBITRATOR_JOIN,
            &join,
        )
        .map_err(|_| DisputeError::MalformedDispute)?;
        let signed = SignedArbitratorJoin {
            signature: self.gossip.sign(&bytes),
            join,
        };
        self.originate(protocol::EVENT_ARBITRATOR_JOINED, &signed)
    }

    /// Commit to `vote`, keeping `secret` for the caller to reveal later
    /// (see [`commitment::compute`]).
    pub fn commit_vote(
        &mut self,
        dispute_id: DisputeId,
        vote: Vote,
        secret: [u8; 32],
    ) -> Result<(), DisputeError> {
        let commit = VoteCommit {
            dispute_id,
            arbitrator: self.gossip.node.local_peer_id(),
            commitment: commitment::compute(vote, &secret),
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::DISPUTE_VOTE_COMMIT,
            &commit,
        )
        .map_err(|_| DisputeError::MalformedDispute)?;
        let signed = SignedVoteCommit {
            signature: self.gossip.sign(&bytes),
            commit,
        };
        self.originate(protocol::EVENT_VOTE_COMMITTED, &signed)
    }

    pub fn reveal_vote(
        &mut self,
        dispute_id: DisputeId,
        vote: Vote,
        secret: [u8; 32],
    ) -> Result<(), DisputeError> {
        let reveal = VoteReveal {
            dispute_id,
            arbitrator: self.gossip.node.local_peer_id(),
            vote,
            secret,
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::DISPUTE_VOTE_REVEAL,
            &reveal,
        )
        .map_err(|_| DisputeError::MalformedDispute)?;
        let signed = SignedVoteReveal {
            signature: self.gossip.sign(&bytes),
            reveal,
        };
        self.originate(protocol::EVENT_VOTE_REVEALED, &signed)
    }

    pub fn agree_to_mutual_settlement(
        &mut self,
        dispute_id: DisputeId,
    ) -> Result<(), DisputeError> {
        let agree = MutualSettlementAgree {
            dispute_id,
            party: self.gossip.node.local_peer_id(),
            timestamp: Timestamp::now(),
        };
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::MUTUAL_SETTLEMENT_AGREE,
            &agree,
        )
        .map_err(|_| DisputeError::MalformedDispute)?;
        let signed = SignedMutualSettlementAgree {
            signature: self.gossip.sign(&bytes),
            agree,
        };
        self.originate(protocol::EVENT_MUTUAL_SETTLEMENT_AGREED, &signed)
    }

    fn originate(
        &mut self,
        event_type: &str,
        payload: &impl serde::Serialize,
    ) -> Result<(), DisputeError> {
        let bytes = wire::to_bytes(payload).map_err(|_| DisputeError::MalformedDispute)?;
        let event_type = EventType::new(event_type)
            .expect("dispute event names are all valid PascalCase identifiers");
        self.gossip
            .originate(
                event_type,
                protocol::OFS_SPEC,
                Priority::Governance,
                8,
                bytes,
            )
            .map(|_| ())
            .map_err(|_| DisputeError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
