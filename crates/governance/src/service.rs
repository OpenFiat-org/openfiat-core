//! Drives one node's governance index: applies incoming gossip events
//! automatically and provides the operations that originate new ones.

use crate::error::GovernanceError;
use crate::events::{ProposalActivate, ProposalCreate, ProposalWithdraw, SignedProposalActivate, SignedProposalCreate, SignedProposalWithdraw, SignedVoteCast, VoteCast};
use crate::protocol;
use crate::record::{Proposal, ProposalCategory, ProposalId, VoteChoice};
use crate::store::GovernanceRegistry;
use openfiat_gossip::GossipService;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventType, Priority, Timestamp};
use std::rc::Rc;

pub struct GovernanceService<S> {
    pub gossip: GossipService<S>,
    registry: Rc<GovernanceRegistry<S>>,
}

impl<S: KvStore + 'static> GovernanceService<S> {
    pub fn new(mut gossip: GossipService<S>, store: S) -> Self {
        let registry = Rc::new(GovernanceRegistry::new(store));
        let handler_registry = Rc::clone(&registry);
        gossip.set_event_handler(move |event| handler_registry.apply_event(event));
        Self { gossip, registry }
    }

    pub fn registry(&self) -> Rc<GovernanceRegistry<S>> {
        Rc::clone(&self.registry)
    }

    pub fn get(&self, id: &ProposalId) -> Option<Proposal> {
        self.registry.get(id)
    }

    pub fn all(&self) -> Vec<Proposal> {
        self.registry.all()
    }

    /// Every node resolves proposals whose voting window has passed
    /// purely from local state — see `GovernanceRegistry::resolve_expired`.
    pub fn resolve_expired(&self) -> usize {
        self.registry.resolve_expired(Timestamp::now())
    }

    pub fn create_proposal(&mut self, id: impl Into<String>, title: impl Into<String>, summary: impl Into<String>, category: ProposalCategory) -> Result<ProposalId, GovernanceError> {
        let create = ProposalCreate {
            id: ProposalId::new(id),
            title: title.into(),
            summary: summary.into(),
            category,
            author: self.gossip.node.local_peer_id(),
            author_public_key: self.gossip.public_key(),
            timestamp: Timestamp::now(),
        };
        let bytes = wire::to_bytes(&create).map_err(|_| GovernanceError::MalformedProposal)?;
        let signed = SignedProposalCreate { signature: self.gossip.sign(&bytes), create };
        self.originate(protocol::EVENT_CREATED, &signed)?;
        Ok(signed.create.id)
    }

    pub fn cast_vote(&mut self, proposal_id: ProposalId, choice: VoteChoice, weight: u64) -> Result<(), GovernanceError> {
        let vote = VoteCast { proposal_id, voter: self.gossip.node.local_peer_id(), voter_public_key: self.gossip.public_key(), choice, weight, timestamp: Timestamp::now() };
        let bytes = wire::to_bytes(&vote).map_err(|_| GovernanceError::MalformedProposal)?;
        let signed = SignedVoteCast { signature: self.gossip.sign(&bytes), vote };
        self.originate(protocol::EVENT_VOTE_CAST, &signed)
    }

    pub fn withdraw_proposal(&mut self, proposal_id: ProposalId) -> Result<(), GovernanceError> {
        let withdraw = ProposalWithdraw { proposal_id, author: self.gossip.node.local_peer_id(), timestamp: Timestamp::now() };
        let bytes = wire::to_bytes(&withdraw).map_err(|_| GovernanceError::MalformedProposal)?;
        let signed = SignedProposalWithdraw { signature: self.gossip.sign(&bytes), withdraw };
        self.originate(protocol::EVENT_WITHDRAWN, &signed)
    }

    pub fn activate_proposal(&mut self, proposal_id: ProposalId) -> Result<(), GovernanceError> {
        let activate = ProposalActivate { proposal_id, author: self.gossip.node.local_peer_id(), timestamp: Timestamp::now() };
        let bytes = wire::to_bytes(&activate).map_err(|_| GovernanceError::MalformedProposal)?;
        let signed = SignedProposalActivate { signature: self.gossip.sign(&bytes), activate };
        self.originate(protocol::EVENT_ACTIVATED, &signed)
    }

    fn originate(&mut self, event_type: &str, payload: &impl serde::Serialize) -> Result<(), GovernanceError> {
        let bytes = wire::to_bytes(payload).map_err(|_| GovernanceError::MalformedProposal)?;
        let event_type = EventType::new(event_type).expect("governance event names are all valid PascalCase identifiers");
        self.gossip
            .originate(event_type, protocol::OFS_SPEC, Priority::Governance, 8, bytes)
            .map(|_| ())
            .map_err(|_| GovernanceError::Unauthorized)
    }

    pub async fn drive_once(&mut self) {
        self.gossip.drive_once().await;
    }
}
