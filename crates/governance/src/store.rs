//! The replicated local governance index (§23: every governance action
//! is publicly recorded; §26: deterministic proposal states).

use crate::error::GovernanceError;
use crate::events::{
    SignedProposalActivate, SignedProposalCreate, SignedProposalWithdraw, SignedVoteCast,
};
use crate::onchain::{ChainAgreement, ProposalChainView};
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
            onchain_proposal_id: create.onchain_proposal_id,
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
    /// # Prefer [`Self::adopt_onchain_resolution`]
    ///
    /// This takes the resolution as an argument and trusts it. It is the
    /// low-level half, kept public for tests and for callers that have
    /// already established the link themselves. Everything reading a real
    /// chain should go through `adopt_onchain_resolution`, which will not
    /// adopt an outcome from an account that has not been joined to this
    /// proposal in both directions.
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

    /// Adopt the chain's answer for `id`, given the on-chain account this
    /// node read for it — the caller [`Self::apply_onchain_resolution`]
    /// was waiting for.
    ///
    /// `onchain` is `None` when the account could not be fetched, was not
    /// a `Proposal`, or was owned by another program. All of those mean
    /// "this node cannot state an outcome", which is not a reason to
    /// invent one.
    ///
    /// Returns what the two records say about each other, so a caller can
    /// distinguish a proposal it adopted from one it declined to adopt
    /// and *why*:
    ///
    /// * [`ChainAgreement::LinkedAwaitingAdoption`] is the case that
    ///   writes: the link holds, the chain has resolved, and local state
    ///   is still `Voting`. This is where the chain's answer lands.
    /// * [`ChainAgreement::LinkedDisagreed`] writes nothing, because
    ///   local state has already left `Voting` — it was adopted,
    ///   withdrawn or activated — and letting a later read flip a settled
    ///   outcome would make the local record depend on poll ordering. The
    ///   return value is what lets the caller surface the divergence
    ///   instead of it passing unnoticed.
    /// * [`ChainAgreement::ClaimNotReciprocated`] writes nothing,
    ///   deliberately. An unreciprocated claim is an outcome belonging to
    ///   some other proposal, and adopting it would let anyone resolve
    ///   anyone's proposal by creating an on-chain one that names it.
    ///
    /// Idempotent: once a proposal has left `Voting`,
    /// `apply_onchain_resolution` refuses to move it again, so a poll may
    /// re-read the same account every tick without effect.
    pub fn adopt_onchain_resolution(
        &self,
        id: &ProposalId,
        onchain: Option<&crate::onchain::OnchainProposal>,
        now: Timestamp,
    ) -> Result<ChainAgreement, GovernanceError> {
        let proposal = self.get(id).ok_or(GovernanceError::ProposalNotFound)?;
        let agreement = crate::onchain::compare(&proposal, onchain);
        if agreement == ChainAgreement::LinkedAwaitingAdoption {
            let resolved = onchain
                .and_then(|onchain| onchain.state.resolution())
                .expect("LinkedAwaitingAdoption implies the chain resolved");
            // `InvalidStateTransition` here means the proposal had
            // already left `Voting` — a re-read of something already
            // adopted, withdrawn or activated. Not an error for a poll.
            match self.apply_onchain_resolution(id, resolved, now) {
                Ok(()) | Err(GovernanceError::InvalidStateTransition) => {}
                Err(other) => return Err(other),
            }
        }
        Ok(agreement)
    }

    /// The off-chain proposal beside the chain's record of it, and the
    /// verdict on whether they agree — the read a client needs in order
    /// to show both rather than one and imply the other.
    ///
    /// Pure: unlike [`Self::adopt_onchain_resolution`] it writes nothing,
    /// so an interface can render the disagreement without the act of
    /// looking resolving it.
    pub fn chain_view(
        &self,
        id: &ProposalId,
        onchain: Option<crate::onchain::OnchainProposal>,
    ) -> Option<ProposalChainView> {
        let offchain = self.get(id)?;
        let agreement = crate::onchain::compare(&offchain, onchain.as_ref());
        Some(ProposalChainView {
            offchain,
            onchain,
            agreement,
        })
    }

    /// Every proposal that claims an on-chain counterpart and has not
    /// been resolved locally yet — i.e. exactly the set a chain-resolution
    /// poll needs to fetch accounts for.
    ///
    /// Returns the off-chain id paired with the on-chain `u64`, because a
    /// poll needs the first to write the answer back and the second to
    /// derive the PDA. Proposals that claim nothing are skipped: there is
    /// no account to read, and a poll that fetched one anyway would be
    /// asking the chain about a proposal that was never put to it.
    pub fn pending_onchain_resolutions(&self) -> Vec<(ProposalId, u64)> {
        self.all()
            .into_iter()
            .filter(|proposal| proposal.status == ProposalStatus::Voting)
            .filter_map(|proposal| {
                proposal
                    .onchain_proposal_id
                    .map(|onchain_id| (proposal.id, onchain_id))
            })
            .collect()
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
    /// is authoritative and this crate adopts it rather than recomputing
    /// it — see [`Self::adopt_onchain_resolution`], and [`crate::onchain`]
    /// for the two-sided join that says which on-chain proposal is this
    /// one.
    ///
    /// A proposal with no on-chain counterpart, or one whose counterpart
    /// this node has not read, stays `Voting` in local state. Callers
    /// wanting to know whether the window has closed should compare
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
        registry_claiming_onchain(author, id, None)
    }

    /// `onchain_proposal_id` is the off-chain half of the join key — the
    /// on-chain `Proposal` this one claims to be.
    fn registry_claiming_onchain(
        author: &Keypair,
        id: &str,
        onchain_proposal_id: Option<u64>,
    ) -> (GovernanceRegistry<MemoryStore>, ProposalId) {
        let registry = GovernanceRegistry::new(MemoryStore::new());
        let create = ProposalCreate {
            id: ProposalId::new(id),
            title: "Increase Reservation Timeout".to_string(),
            summary: "Raise the validation window from 30 to 45 minutes.".to_string(),
            category: ProposalCategory::Protocol,
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            onchain_proposal_id,
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
            onchain_proposal_id: None,
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

    /// An on-chain `Proposal` account's raw bytes, built the same way
    /// `crate::onchain`'s own tests build them, so these exercise the
    /// real decoder rather than a hand-made struct.
    fn onchain_proposal(
        id: u64,
        state: u8,
        offchain_id_hash: [u8; 32],
    ) -> crate::onchain::OnchainProposal {
        // discriminator(8) + the rest of the layout, all zero except the
        // fields these tests care about.
        let mut bytes = vec![0u8; 200];
        bytes[..8].copy_from_slice(&[26, 94, 189, 187, 116, 136, 53, 33]);
        bytes[8..16].copy_from_slice(&id.to_le_bytes());
        bytes[163] = state;
        bytes[164] = 1; // quorum_met
        bytes[168..200].copy_from_slice(&offchain_id_hash);
        crate::onchain::decode_proposal(crate::onchain::GOVERNANCE_PROGRAM_ID, &bytes)
            .expect("the fixture must be a decodable Proposal account")
    }

    #[test]
    fn the_chain_resolves_a_proposal_that_names_it_and_that_it_names_back() {
        // The end-to-end shape of the wiring: a proposal claiming
        // on-chain id 7, an on-chain proposal 7 claiming it back, and the
        // chain's Accepted becoming the local status. Before this, the
        // status could only be set by a caller that did not exist.
        let author = Keypair::generate();
        let (registry, id) = registry_claiming_onchain(&author, "ofip-0001", Some(7));
        let onchain = onchain_proposal(7, 2, crate::onchain::offchain_id_hash(&id));

        let agreement = registry
            .adopt_onchain_resolution(&id, Some(&onchain), Timestamp::now())
            .unwrap();
        assert_eq!(
            agreement,
            crate::onchain::ChainAgreement::LinkedAwaitingAdoption
        );
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Accepted);
    }

    #[test]
    fn a_chain_outcome_belonging_to_another_proposal_resolves_nothing() {
        // The attack the two-sided join exists to stop: anyone may create
        // an on-chain proposal, so if a one-sided claim were enough, a
        // stranger could resolve somebody else's proposal by creating an
        // on-chain one and letting the poll adopt its tally.
        let author = Keypair::generate();
        let (registry, id) = registry_claiming_onchain(&author, "ofip-0001", Some(7));
        let unrelated = onchain_proposal(
            7,
            2,
            crate::onchain::offchain_id_hash(&ProposalId::new("ofip-9999")),
        );

        let agreement = registry
            .adopt_onchain_resolution(&id, Some(&unrelated), Timestamp::now())
            .unwrap();
        assert_eq!(
            agreement,
            crate::onchain::ChainAgreement::ClaimNotReciprocated
        );
        assert_eq!(
            registry.get(&id).unwrap().status,
            ProposalStatus::Voting,
            "an unreciprocated claim must not move the status"
        );
    }

    #[test]
    fn a_node_that_could_not_read_the_account_resolves_nothing() {
        // A `GossipOnly` node, or one whose RPC call failed. It must stay
        // at "I do not know" rather than fall back to a local guess —
        // the same principle `local_vote_preview` exists to protect.
        let author = Keypair::generate();
        let (registry, id) = registry_claiming_onchain(&author, "ofip-0001", Some(7));
        let agreement = registry
            .adopt_onchain_resolution(&id, None, Timestamp::now())
            .unwrap();
        assert_eq!(
            agreement,
            crate::onchain::ChainAgreement::ClaimNotReciprocated
        );
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Voting);
    }

    #[test]
    fn re_reading_the_same_account_is_harmless() {
        // A poll re-reads every tick. The second adoption must not be an
        // error, or a poll would log a failure on every proposal it had
        // already settled.
        let author = Keypair::generate();
        let (registry, id) = registry_claiming_onchain(&author, "ofip-0001", Some(7));
        let onchain = onchain_proposal(7, 3, crate::onchain::offchain_id_hash(&id));
        for _ in 0..3 {
            registry
                .adopt_onchain_resolution(&id, Some(&onchain), Timestamp::now())
                .unwrap();
        }
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Rejected);
    }

    #[test]
    fn the_chain_overrides_a_local_status_that_contradicts_it() {
        // The disagreement case, and the reason the return value exists:
        // the chain wins, and the caller is told a divergence happened
        // rather than the overwrite being silent.
        let author = Keypair::generate();
        let (registry, id) = registry_claiming_onchain(&author, "ofip-0001", Some(7));
        let hash = crate::onchain::offchain_id_hash(&id);
        registry
            .adopt_onchain_resolution(&id, Some(&onchain_proposal(7, 2, hash)), Timestamp::now())
            .unwrap();
        assert_eq!(registry.get(&id).unwrap().status, ProposalStatus::Accepted);

        // The chain now says Rejected for the same proposal. Local state
        // has already left `Voting`, so it stays put — an accepted
        // proposal is not un-accepted by a later read, which would
        // otherwise let a stale or reordered chain read flip a settled
        // outcome. The divergence is still reported.
        let agreement = registry
            .adopt_onchain_resolution(&id, Some(&onchain_proposal(7, 3, hash)), Timestamp::now())
            .unwrap();
        assert_eq!(agreement, crate::onchain::ChainAgreement::LinkedDisagreed);
    }

    #[test]
    fn a_poll_is_only_asked_to_fetch_proposals_that_claim_a_counterpart() {
        let author = Keypair::generate();
        let (registry, claiming) = registry_claiming_onchain(&author, "ofip-0001", Some(42));
        // A second proposal in the same registry that never went on chain.
        let create = ProposalCreate {
            id: ProposalId::new("ofip-0002"),
            title: "Purely informational".to_string(),
            summary: "Never goes on chain.".to_string(),
            category: ProposalCategory::Governance,
            author: peer_id_from_public_key(&author.public_key()).unwrap(),
            author_public_key: author.public_key(),
            onchain_proposal_id: None,
            timestamp: Timestamp::now(),
        };
        registry
            .apply_create(SignedProposalCreate::sign(create, &author))
            .unwrap();

        assert_eq!(
            registry.pending_onchain_resolutions(),
            vec![(claiming.clone(), 42)]
        );

        // Once resolved it drops out, so a poll's workload shrinks as
        // proposals settle rather than growing forever.
        registry
            .adopt_onchain_resolution(
                &claiming,
                Some(&onchain_proposal(
                    42,
                    2,
                    crate::onchain::offchain_id_hash(&claiming),
                )),
                Timestamp::now(),
            )
            .unwrap();
        assert!(registry.pending_onchain_resolutions().is_empty());
    }

    #[test]
    fn a_chain_view_shows_both_records_rather_than_one_and_an_implication() {
        // What a client gets: the off-chain record, the chain's record,
        // and an explicit verdict — instead of one record presented as if
        // it were both.
        let author = Keypair::generate();
        let (registry, id) = registry_claiming_onchain(&author, "ofip-0001", Some(7));
        let onchain = onchain_proposal(7, 2, crate::onchain::offchain_id_hash(&id));

        let view = registry.chain_view(&id, Some(onchain.clone())).unwrap();
        assert_eq!(view.offchain.status, ProposalStatus::Voting);
        assert_eq!(view.onchain.as_ref().unwrap().id, 7);
        assert_eq!(
            view.agreement,
            crate::onchain::ChainAgreement::LinkedAwaitingAdoption,
            "the chain has decided but this node has not adopted it yet"
        );
        assert_eq!(
            registry.get(&id).unwrap().status,
            ProposalStatus::Voting,
            "reading the view must not resolve anything"
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
