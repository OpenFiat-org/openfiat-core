//! The replicated local governance index (§23: every governance action
//! is publicly recorded; §26: deterministic proposal states).

use crate::error::GovernanceError;
use crate::events::{
    SignedProposalActivate, SignedProposalCreate, SignedProposalWithdraw, SignedVoteCast,
};
use crate::protocol;
use crate::record::{CastVote, Proposal, ProposalId, ProposalStatus};
use openfiat_crypto::verify;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, Timestamp};

const COLUMN_FAMILY: &str = "governance_proposals";

pub struct GovernanceRegistry<S> {
    store: S,
}

impl<S: KvStore> GovernanceRegistry<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn get(&self, id: &ProposalId) -> Option<Proposal> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, proposal: &Proposal) {
        if let Ok(bytes) = wire::to_bytes(proposal) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, proposal.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Proposal> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    pub fn apply_create(
        &self,
        signed: SignedProposalCreate,
    ) -> Result<ProposalId, GovernanceError> {
        signed.verify()?;
        let id = signed.create.id.clone();
        if self.get(&id).is_some() {
            return Err(GovernanceError::DuplicateProposalId);
        }
        let create = signed.create;
        let voting_closes_at = Timestamp::from_millis(
            create.timestamp.as_millis() + protocol::DEFAULT_VOTING_PERIOD.as_millis() as u64,
        );
        self.put(&Proposal {
            id: id.clone(),
            title: create.title,
            summary: create.summary,
            category: create.category,
            author: create.author,
            author_public_key: create.author_public_key,
            status: ProposalStatus::Voting,
            votes: Vec::new(),
            voting_closes_at,
            created_at: create.timestamp,
            updated_at: create.timestamp,
        });
        Ok(id)
    }

    /// §13/§24: only legal while voting is open, and only once per voter.
    /// Trusts the vote's own self-reported `weight` — safe only for a
    /// caller with no independent way to check it (this crate has no
    /// chain connectivity of its own). The shipped node instead calls
    /// [`Self::apply_vote_with_verified_weight`].
    pub fn apply_vote(&self, signed: SignedVoteCast) -> Result<(), GovernanceError> {
        signed.verify()?;
        let weight = signed.vote.weight;
        self.apply_vote_inner(signed, weight)
    }

    /// Same as [`Self::apply_vote`], but with `weight` overridden by a
    /// caller-supplied value rather than the vote's own self-reported
    /// one — used once that value has been independently confirmed
    /// against real on-chain stake (`crates/rpc::actor::
    /// poll_vote_verifications` + `crates/rpc::onchain_stake`), closing
    /// this crate's previously-documented trust gap.
    pub fn apply_vote_with_verified_weight(
        &self,
        signed: SignedVoteCast,
        verified_weight: u64,
    ) -> Result<(), GovernanceError> {
        signed.verify()?;
        self.apply_vote_inner(signed, verified_weight)
    }

    fn apply_vote_inner(&self, signed: SignedVoteCast, weight: u64) -> Result<(), GovernanceError> {
        let mut proposal = self
            .get(&signed.vote.proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        if proposal.status != ProposalStatus::Voting
            || signed.vote.timestamp.as_millis() >= proposal.voting_closes_at.as_millis()
        {
            return Err(GovernanceError::VotingClosed);
        }
        if proposal.vote_by(&signed.vote.voter).is_some() {
            return Err(GovernanceError::DuplicateVote);
        }

        proposal.votes.push(CastVote {
            voter: signed.vote.voter,
            choice: signed.vote.choice,
            weight,
            timestamp: signed.vote.timestamp,
        });
        proposal.updated_at = signed.vote.timestamp;
        self.put(&proposal);
        Ok(())
    }

    /// §21: proposal authors may withdraw before voting concludes.
    pub fn apply_withdraw(&self, signed: SignedProposalWithdraw) -> Result<(), GovernanceError> {
        let mut proposal = self
            .get(&signed.withdraw.proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        if proposal.author != signed.withdraw.author {
            return Err(GovernanceError::Unauthorized);
        }
        let bytes =
            json::to_bytes(&signed.withdraw).map_err(|_| GovernanceError::MalformedProposal)?;
        verify(&proposal.author_public_key, &bytes, &signed.signature)
            .map_err(|_| GovernanceError::InvalidSignature)?;
        if proposal.status != ProposalStatus::Voting {
            return Err(GovernanceError::InvalidStateTransition);
        }

        proposal.status = ProposalStatus::Withdrawn;
        proposal.updated_at = signed.withdraw.timestamp;
        self.put(&proposal);
        Ok(())
    }

    /// §18: only a proposal that reached `Accepted` may be activated.
    pub fn apply_activate(&self, signed: SignedProposalActivate) -> Result<(), GovernanceError> {
        let mut proposal = self
            .get(&signed.activate.proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        if proposal.author != signed.activate.author {
            return Err(GovernanceError::Unauthorized);
        }
        let bytes =
            json::to_bytes(&signed.activate).map_err(|_| GovernanceError::MalformedProposal)?;
        verify(&proposal.author_public_key, &bytes, &signed.signature)
            .map_err(|_| GovernanceError::InvalidSignature)?;
        if proposal.status != ProposalStatus::Accepted {
            return Err(GovernanceError::InvalidStateTransition);
        }

        proposal.status = ProposalStatus::Activated;
        proposal.updated_at = signed.activate.timestamp;
        self.put(&proposal);
        Ok(())
    }

    /// Adopt the resolution the governance program has already reached.
    ///
    /// This is the only way a proposal becomes `Accepted` or `Rejected`.
    /// The status is an *input* here, read from the chain, rather than
    /// something this crate computes — which is the whole distinction from
    /// the local tally this replaced. Every node that can read the chain
    /// adopts the same value, so nodes agree by construction instead of by
    /// coincidence.
    ///
    /// Only `Accepted` and `Rejected` are adoptable. `Withdrawn` and
    /// `Activated` are off-chain lifecycle states the program knows nothing
    /// about, and `Voting` is not a resolution.
    ///
    /// # Its caller does not exist yet, and why
    ///
    /// The chain-resolution poll that should call this needs to find the
    /// on-chain proposal corresponding to an off-chain [`ProposalId`], and
    /// **there is no link between them**: off-chain ids are strings chosen
    /// by the author, on-chain proposals are keyed by a `u64`, and the
    /// on-chain record holds only `title_hash`/`summary_hash` whose hash
    /// function is not pinned anywhere. Establishing that link changes a
    /// signed event's wire format, so it is a protocol decision rather than
    /// an implementation detail.
    ///
    /// This is a seam waiting on that decision, not a feature pretending to
    /// work. Until it is called, proposals stay `Voting` locally — which is
    /// exactly what they did before, since the local tally was never wired
    /// into the running node either.
    pub fn apply_onchain_resolution(
        &self,
        id: &ProposalId,
        resolved: ProposalStatus,
        now: Timestamp,
    ) -> Result<(), GovernanceError> {
        if !matches!(
            resolved,
            ProposalStatus::Accepted | ProposalStatus::Rejected
        ) {
            return Err(GovernanceError::InvalidStateTransition);
        }
        let mut proposal = self.get(id).ok_or(GovernanceError::ProposalNotFound)?;
        if proposal.status != ProposalStatus::Voting {
            // Already resolved, withdrawn or activated. Re-adopting would
            // let a stale chain read undo a later local transition.
            return Err(GovernanceError::InvalidStateTransition);
        }
        proposal.status = resolved;
        proposal.updated_at = now;
        self.put(&proposal);
        Ok(())
    }

    /// An **unverified preview** of how the votes this node happens to
    /// hold would tally. Never a resolution.
    ///
    /// # Why this cannot decide a proposal
    ///
    /// A vote only enters [`Proposal::votes`] once its weight has been read
    /// from the voter's on-chain `StakeAccount`, which needs an RPC
    /// endpoint. A `GossipOnly` node discards every vote it cannot verify.
    /// So the vote set is a function of this node's connectivity, and
    /// tallying it produces a per-node answer:
    ///
    /// - An `RpcConnected` node holds the verified votes and would say
    ///   accepted.
    /// - A `GossipOnly` node holds none, fails
    ///   [`protocol::MINIMUM_VOTERS_FOR_QUORUM`], and would say **rejected**.
    ///
    /// Both nodes are behaving correctly and they disagree. Worse, the
    /// second cannot tell "nobody voted" from "I discarded the votes I
    /// could not verify", so it would report a definite rejection where the
    /// truthful answer is "I do not know". That is not a disagreement to
    /// reconcile later; it is a confident wrong answer.
    ///
    /// This used to be `resolve_expired`, which wrote the tally straight
    /// into [`Proposal::status`]. It was never called in production — only
    /// by `GovernanceService` and tests — so nothing regressed when it
    /// stopped deciding, but wiring it in as it stood would have shipped
    /// exactly the divergence above.
    ///
    /// # Where a resolution actually comes from
    ///
    /// The governance program's `tally_and_finalize` already decides, on
    /// chain, from stake-weighted votes every node can verify. That result
    /// is authoritative and this crate should adopt it rather than
    /// recompute it. Adopting it needs a link from an off-chain
    /// [`ProposalId`] to the on-chain proposal's `u64` id, which does not
    /// exist yet — see the note in `protocol`.
    ///
    /// Until then a proposal stays `Voting` in local state. Callers wanting
    /// to know whether the window has closed should compare
    /// [`Proposal::voting_closes_at`] against the clock, which is a fact
    /// this node can establish on its own, rather than reading a status it
    /// cannot substantiate.
    pub fn local_vote_preview(&self, id: &ProposalId, now: Timestamp) -> Option<VotePreview> {
        let proposal = self.get(id)?;
        let (mut approve, mut reject, mut abstain) = (0u64, 0u64, 0u64);
        for vote in &proposal.votes {
            match vote.choice {
                crate::record::VoteChoice::Approve => approve += vote.weight,
                crate::record::VoteChoice::Reject => reject += vote.weight,
                crate::record::VoteChoice::Abstain => abstain += vote.weight,
            }
        }
        Some(VotePreview {
            voters_seen: proposal.votes.len(),
            approve_weight: approve,
            reject_weight: reject,
            abstain_weight: abstain,
            // A fact this node can establish alone, unlike the outcome.
            voting_closed: now.as_millis() >= proposal.voting_closes_at.as_millis(),
        })
    }

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
            protocol::EVENT_VOTE_CAST => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_vote(signed);
                }
            }
            protocol::EVENT_WITHDRAWN => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_withdraw(signed);
                }
            }
            protocol::EVENT_ACTIVATED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_activate(signed);
                }
            }
            _ => {}
        }
    }
}

/// §15-16: quorum (a minimum number of distinct voters) and a simple
/// majority of weight among `Approve`/`Reject` votes; a quorum miss or a
/// genuine weight tie both resolve to `Rejected` as a safe, deterministic
/// fallback, the same tie-breaking philosophy as disputes' consensus.
/// What the votes a node currently holds add up to.
///
/// Deliberately **not** a [`ProposalStatus`]. There is no
/// `VotePreview -> ProposalStatus` conversion anywhere and there should not
/// be one: the whole point is that this cannot become a resolution by
/// accident. A function returning `ProposalStatus` from local votes existed
/// here before and is what made the divergence in
/// [`GovernanceRegistry::local_vote_preview`] possible.
///
/// `voters_seen` is the honest name for the count: it is how many votes
/// *this node* holds and verified, not how many were cast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VotePreview {
    pub voters_seen: usize,
    pub approve_weight: u64,
    pub reject_weight: u64,
    pub abstain_weight: u64,
    /// Whether the voting window has closed. Derived from the clock and
    /// [`Proposal::voting_closes_at`], so unlike the outcome it is
    /// something a node can establish without asking anyone.
    pub voting_closed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ProposalActivate, ProposalCreate, ProposalWithdraw, VoteCast};
    use crate::record::{ProposalCategory, VoteChoice};
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;

    fn registry_with_proposal(
        author: &Keypair,
        id: &str,
    ) -> (GovernanceRegistry<MemoryStore>, ProposalId) {
        let registry = GovernanceRegistry::new(MemoryStore::new());
        let create = ProposalCreate {
            id: ProposalId::new(id),
            title: "Increase Reservation Timeout".to_string(),
            summary: "Raise the validation window from 30 to 45 minutes.".to_string(),
            category: ProposalCategory::Protocol,
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            timestamp: Timestamp::now(),
        };
        let id = registry
            .apply_create(SignedProposalCreate::sign(create, author))
            .unwrap();
        (registry, id)
    }

    fn cast(
        registry: &GovernanceRegistry<MemoryStore>,
        proposal_id: &ProposalId,
        voter: &Keypair,
        choice: VoteChoice,
        weight: u64,
    ) -> Result<(), GovernanceError> {
        let vote = VoteCast {
            proposal_id: proposal_id.clone(),
            voter: peer_id_from_public_key(&voter.public_key()).unwrap(),
            voter_public_key: voter.public_key(),
            choice,
            weight,
            stake_account: String::new(),
            timestamp: Timestamp::now(),
        };
        registry.apply_vote(SignedVoteCast::sign(vote, voter))
    }

    #[test]
    fn a_created_proposal_opens_directly_for_voting() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Voting);
    }

    #[test]
    fn duplicate_creation_is_rejected() {
        let author = Keypair::generate();
        let registry = GovernanceRegistry::new(MemoryStore::new());
        let create = || ProposalCreate {
            id: ProposalId::new("ofp-1"),
            title: "T".to_string(),
            summary: "S".to_string(),
            category: ProposalCategory::Protocol,
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            timestamp: Timestamp::now(),
        };
        registry
            .apply_create(SignedProposalCreate::sign(create(), &author))
            .unwrap();
        let result = registry.apply_create(SignedProposalCreate::sign(create(), &author));
        assert_eq!(result, Err(GovernanceError::DuplicateProposalId));
    }

    #[test]
    fn a_voter_cannot_vote_twice() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let voter = Keypair::generate();
        cast(&registry, &id, &voter, VoteChoice::Approve, 10).unwrap();
        let result = cast(&registry, &id, &voter, VoteChoice::Reject, 10);
        assert_eq!(result, Err(GovernanceError::DuplicateVote));
    }

    #[test]
    fn verified_weight_overrides_whatever_the_vote_itself_self_reports() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let voter = Keypair::generate();
        let vote = VoteCast {
            proposal_id: id.clone(),
            voter: peer_id_from_public_key(&voter.public_key()).unwrap(),
            voter_public_key: voter.public_key(),
            choice: VoteChoice::Approve,
            weight: 999_999, // an unverified, self-reported lie
            stake_account: "stake-account-1".to_string(),
            timestamp: Timestamp::now(),
        };
        registry
            .apply_vote_with_verified_weight(SignedVoteCast::sign(vote, &voter), 42)
            .unwrap();

        let recorded = registry
            .get(&id)
            .unwrap()
            .vote_by(&peer_id_from_public_key(&voter.public_key()).unwrap())
            .unwrap()
            .weight;
        assert_eq!(recorded, 42);
    }

    #[test]
    fn a_preview_reports_the_weights_this_node_holds() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let voters: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
        cast(&registry, &id, &voters[0], VoteChoice::Approve, 10).unwrap();
        cast(&registry, &id, &voters[1], VoteChoice::Approve, 5).unwrap();
        cast(&registry, &id, &voters[2], VoteChoice::Reject, 3).unwrap();

        let far_future =
            Timestamp::from_millis(registry.get(&id).unwrap().voting_closes_at.as_millis() + 1);
        let preview = registry.local_vote_preview(&id, far_future).unwrap();
        assert_eq!(preview.voters_seen, 3);
        assert_eq!(preview.approve_weight, 15);
        assert_eq!(preview.reject_weight, 3);
        assert!(preview.voting_closed);
    }

    #[test]
    fn a_preview_never_becomes_a_resolution() {
        // The property this whole change exists for. Previously this same
        // setup wrote Accepted into the proposal's status, and the mirror
        // case — a node holding too few votes — wrote Rejected. A node
        // without an RPC endpoint holds NO verified votes, so it would have
        // written Rejected for every proposal, reporting a definite outcome
        // where the honest answer is "I cannot tell". Status must only ever
        // come from the chain's own tally.
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let voter = Keypair::generate();
        cast(&registry, &id, &voter, VoteChoice::Approve, 1000).unwrap();

        let far_future =
            Timestamp::from_millis(registry.get(&id).unwrap().voting_closes_at.as_millis() + 1);

        // A lone voter is below MINIMUM_VOTERS_FOR_QUORUM, which is exactly
        // the shape that used to resolve to Rejected.
        assert!(registry.get(&id).unwrap().votes.len() < protocol::MINIMUM_VOTERS_FOR_QUORUM);
        let preview = registry.local_vote_preview(&id, far_future).unwrap();
        assert!(preview.voting_closed, "the window really has closed");

        assert_eq!(
            registry.get(&id).unwrap().status,
            ProposalStatus::Voting,
            "reading a preview must not move the status; only the chain resolves"
        );
    }

    #[test]
    fn a_node_holding_no_votes_does_not_report_a_rejection() {
        // The GossipOnly case, stated directly. Such a node discards every
        // vote it cannot verify against on-chain stake, so it holds none —
        // indistinguishable from nobody having voted. It must not turn that
        // into an outcome.
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let far_future =
            Timestamp::from_millis(registry.get(&id).unwrap().voting_closes_at.as_millis() + 1);

        let preview = registry.local_vote_preview(&id, far_future).unwrap();
        assert_eq!(preview.voters_seen, 0);
        assert_eq!(preview.approve_weight, 0);
        assert_eq!(preview.reject_weight, 0);
        assert_eq!(
            registry.get(&id).unwrap().status,
            ProposalStatus::Voting,
            "zero verifiable votes is not a rejection"
        );
    }

    #[test]
    fn withdrawal_by_a_non_author_is_rejected() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let stranger = Keypair::generate();
        let withdraw = ProposalWithdraw {
            proposal_id: id,
            author: peer_id_from_public_key(&stranger.public_key()).unwrap(),
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_withdraw(SignedProposalWithdraw::sign(withdraw, &stranger));
        assert_eq!(result, Err(GovernanceError::Unauthorized));
    }

    #[test]
    fn activation_requires_accepted_status() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let author_peer_id = peer_id_from_public_key(&author.public_key()).unwrap();
        let activate = ProposalActivate {
            proposal_id: id,
            author: author_peer_id,
            timestamp: Timestamp::now(),
        };
        let result = registry.apply_activate(SignedProposalActivate::sign(activate, &author));
        assert_eq!(result, Err(GovernanceError::InvalidStateTransition));
    }

    #[test]
    fn the_full_lifecycle_reaches_activated() {
        let author = Keypair::generate();
        let (registry, id) = registry_with_proposal(&author, "ofp-1");
        let voters: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
        for voter in &voters {
            cast(&registry, &id, voter, VoteChoice::Approve, 1).unwrap();
        }
        let far_future =
            Timestamp::from_millis(registry.get(&id).unwrap().voting_closes_at.as_millis() + 1);
        // Acceptance now arrives from the chain's own tally rather than being
        // computed here. Standing in for the resolution poll, which is
        // blocked on the off-chain-to-on-chain id link.
        registry
            .apply_onchain_resolution(&id, ProposalStatus::Accepted, far_future)
            .unwrap();
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Accepted);

        let author_peer_id = peer_id_from_public_key(&author.public_key()).unwrap();
        let activate = ProposalActivate {
            proposal_id: id.clone(),
            author: author_peer_id,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_activate(SignedProposalActivate::sign(activate, &author))
            .unwrap();
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Activated);
    }
}
