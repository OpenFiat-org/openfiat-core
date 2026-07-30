//! The replicated local dispute index, sharing a handle to the node's
//! settlement registry (§8: a dispute's buyer/seller/amount are the
//! settlement's — not re-declared and separately trusted, but copied
//! from the already-verified settlement record at open time).

use crate::commitment;
use crate::error::DisputeError;
use crate::events::{
    SignedArbitratorJoin, SignedDisputeOpen, SignedMutualSettlementAgree, SignedVoteCommit,
    SignedVoteReveal,
};
use crate::protocol;
use crate::record::{
    ArbitratorCommitment, ArbitratorReveal, Dispute, DisputeId, DisputeStatus, Resolution,
};
use openfiat_crypto::verify;
use openfiat_serialization::json;
use openfiat_serialization::wire;
use openfiat_settlement::SettlementRegistry;
use openfiat_storage::KvStore;
use openfiat_types::EventEnvelope;
use std::rc::Rc;

const COLUMN_FAMILY: &str = "disputes";

pub struct DisputeRegistry<S> {
    store: S,
    settlements: Rc<SettlementRegistry<S>>,
}

impl<S: KvStore> DisputeRegistry<S> {
    pub fn new(store: S, settlements: Rc<SettlementRegistry<S>>) -> Self {
        Self { store, settlements }
    }

    pub fn get(&self, id: &DisputeId) -> Option<Dispute> {
        let bytes = self
            .store
            .get(COLUMN_FAMILY, id.as_str().as_bytes())
            .ok()
            .flatten()?;
        wire::from_bytes(&bytes).ok()
    }

    fn put(&self, dispute: &Dispute) {
        if let Ok(bytes) = wire::to_bytes(dispute) {
            let _ = self
                .store
                .put(COLUMN_FAMILY, dispute.id.as_str().as_bytes(), &bytes);
        }
    }

    pub fn all(&self) -> Vec<Dispute> {
        self.store
            .iter_prefix(COLUMN_FAMILY, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect()
    }

    /// §5-8: only a party to the referenced settlement may open a
    /// dispute on it; buyer/seller/keys are copied from that
    /// already-verified record.
    pub fn apply_open(&self, signed: SignedDisputeOpen) -> Result<DisputeId, DisputeError> {
        signed.verify()?;
        let id = signed.open.id.clone();
        if self.get(&id).is_some() {
            return Err(DisputeError::DuplicateDisputeId);
        }
        let settlement = self
            .settlements
            .get(&signed.open.settlement_id)
            .ok_or(DisputeError::SettlementNotFound)?;
        if signed.open.opener != settlement.buyer && signed.open.opener != settlement.seller {
            return Err(DisputeError::NotAParty);
        }

        self.put(&Dispute {
            id: id.clone(),
            settlement_id: signed.open.settlement_id,
            buyer: settlement.buyer,
            buyer_public_key: settlement.buyer_public_key,
            seller: settlement.seller,
            seller_public_key: settlement.seller_public_key,
            opener: signed.open.opener,
            reason: signed.open.reason,
            status: DisputeStatus::Open,
            required_arbitrators: protocol::REQUIRED_ARBITRATORS,
            arbitrators: Vec::new(),
            arbitrator_keys: Vec::new(),
            commitments: Vec::new(),
            reveals: Vec::new(),
            resolution: None,
            buyer_agreed_mutual_settlement: false,
            seller_agreed_mutual_settlement: false,
            onchain_execution_signature: None,
            opened_at: signed.open.timestamp,
            updated_at: signed.open.timestamp,
        });
        Ok(id)
    }

    /// §14, §16: joining is only legal while the case is `Open`; reaching
    /// `required_arbitrators` locks it immediately so "no further
    /// arbitrators may join".
    pub fn apply_arbitrator_join(&self, signed: SignedArbitratorJoin) -> Result<(), DisputeError> {
        signed.verify()?;
        let mut dispute = self
            .get(&signed.join.dispute_id)
            .ok_or(DisputeError::DisputeNotFound)?;
        if dispute.status != DisputeStatus::Open {
            return Err(DisputeError::InvalidStateTransition);
        }
        if dispute.arbitrators.contains(&signed.join.arbitrator) {
            return Ok(()); // already joined; idempotent no-op (§24 duplicate handling)
        }
        if dispute.arbitrators.len() >= dispute.required_arbitrators as usize {
            return Err(DisputeError::ArbitrationFull);
        }

        dispute.arbitrators.push(signed.join.arbitrator.clone());
        dispute
            .arbitrator_keys
            .push((signed.join.arbitrator, signed.join.arbitrator_public_key));
        if dispute.arbitrators.len() == dispute.required_arbitrators as usize {
            dispute.status = DisputeStatus::CaseLocked;
        }
        dispute.updated_at = signed.join.timestamp;
        self.put(&dispute);
        Ok(())
    }

    /// §16: only legal once the case is locked, and only from a joined
    /// arbitrator, verified against the public key recorded when they
    /// joined.
    pub fn apply_vote_commit(&self, signed: SignedVoteCommit) -> Result<(), DisputeError> {
        let mut dispute = self
            .get(&signed.commit.dispute_id)
            .ok_or(DisputeError::DisputeNotFound)?;
        if dispute.status != DisputeStatus::CaseLocked {
            return Err(DisputeError::InvalidStateTransition);
        }
        let arbitrator_key = dispute
            .arbitrator_key(&signed.commit.arbitrator)
            .copied()
            .ok_or(DisputeError::NotAnArbitrator)?;
        let bytes = json::to_bytes(&signed.commit).map_err(|_| DisputeError::MalformedDispute)?;
        verify(&arbitrator_key, &bytes, &signed.signature)
            .map_err(|_| DisputeError::InvalidSignature)?;

        if dispute
            .commitments
            .iter()
            .any(|c| c.arbitrator == signed.commit.arbitrator)
        {
            return Ok(()); // idempotent no-op
        }
        dispute.commitments.push(ArbitratorCommitment {
            arbitrator: signed.commit.arbitrator,
            commitment: signed.commit.commitment,
        });
        if dispute.commitments.len() == dispute.required_arbitrators as usize {
            dispute.status = DisputeStatus::RevealPhase;
        }
        dispute.updated_at = signed.commit.timestamp;
        self.put(&dispute);
        Ok(())
    }

    /// §16: "only revealed votes matching their earlier commitment are
    /// counted" — a mismatch is rejected outright rather than silently
    /// ignored, so the caller knows the reveal didn't count.
    pub fn apply_vote_reveal(&self, signed: SignedVoteReveal) -> Result<(), DisputeError> {
        let mut dispute = self
            .get(&signed.reveal.dispute_id)
            .ok_or(DisputeError::DisputeNotFound)?;
        if dispute.status != DisputeStatus::RevealPhase {
            return Err(DisputeError::InvalidStateTransition);
        }
        let arbitrator_key = dispute
            .arbitrator_key(&signed.reveal.arbitrator)
            .copied()
            .ok_or(DisputeError::NotAnArbitrator)?;
        let bytes = json::to_bytes(&signed.reveal).map_err(|_| DisputeError::MalformedDispute)?;
        verify(&arbitrator_key, &bytes, &signed.signature)
            .map_err(|_| DisputeError::InvalidSignature)?;

        let committed = dispute
            .commitments
            .iter()
            .find(|c| c.arbitrator == signed.reveal.arbitrator)
            .ok_or(DisputeError::NotAnArbitrator)?;
        if commitment::compute(signed.reveal.vote, &signed.reveal.secret) != committed.commitment {
            return Err(DisputeError::CommitmentMismatch);
        }

        if dispute
            .reveals
            .iter()
            .any(|r| r.arbitrator == signed.reveal.arbitrator)
        {
            return Ok(()); // idempotent no-op
        }
        dispute.reveals.push(ArbitratorReveal {
            arbitrator: signed.reveal.arbitrator,
            vote: signed.reveal.vote,
        });
        if dispute.reveals.len() == dispute.required_arbitrators as usize {
            // Reveals are recorded, never tallied. See
            // `DisputeStatus::AwaitingChainVerdict` for why this node does
            // not decide the case it just finished collecting votes for.
            dispute.status = DisputeStatus::AwaitingChainVerdict;
        }
        dispute.updated_at = signed.reveal.timestamp;
        self.put(&dispute);
        Ok(())
    }

    /// §17: parties may voluntarily resolve at any point before
    /// arbitration concludes.
    pub fn apply_mutual_settlement_agree(
        &self,
        signed: SignedMutualSettlementAgree,
    ) -> Result<(), DisputeError> {
        let mut dispute = self
            .get(&signed.agree.dispute_id)
            .ok_or(DisputeError::DisputeNotFound)?;
        if dispute.status == DisputeStatus::Resolved {
            return Err(DisputeError::InvalidStateTransition);
        }

        let bytes = json::to_bytes(&signed.agree).map_err(|_| DisputeError::MalformedDispute)?;
        if signed.agree.party == dispute.buyer {
            verify(&dispute.buyer_public_key, &bytes, &signed.signature)
                .map_err(|_| DisputeError::InvalidSignature)?;
            dispute.buyer_agreed_mutual_settlement = true;
        } else if signed.agree.party == dispute.seller {
            verify(&dispute.seller_public_key, &bytes, &signed.signature)
                .map_err(|_| DisputeError::InvalidSignature)?;
            dispute.seller_agreed_mutual_settlement = true;
        } else {
            return Err(DisputeError::NotAParty);
        }

        if dispute.buyer_agreed_mutual_settlement && dispute.seller_agreed_mutual_settlement {
            dispute.resolution = Some(Resolution::MutualSettlement);
            dispute.status = DisputeStatus::Resolved;
        }
        dispute.updated_at = signed.agree.timestamp;
        self.put(&dispute);
        Ok(())
    }

    /// Records that this dispute's on-chain `execute_dispute_outcome`
    /// transaction has been independently observed as confirmed (Phase
    /// 4b's dispute-to-chain bridge) — local bookkeeping, not gossiped,
    /// mirroring `SettlementRegistry::apply_escrow_released` exactly:
    /// every node can verify chain confirmation for itself.
    /// Records the outcome the chain decided, and the transaction this
    /// node independently observed confirming it.
    ///
    /// The **only** path that sets a resolution. Everything before this is
    /// the off-chain layer collecting signed votes and relaying them; what
    /// those votes add up to is the chain's answer, computed under the
    /// chain's rules — stake-weighted, quorum-floored, re-opening a round
    /// rather than breaking a tie — and reading it here rather than
    /// re-deriving it is what keeps the two from disagreeing.
    ///
    /// `outcome` is `None` when this node observed the execution but
    /// could not read what it decided — see the body for why that is
    /// recorded honestly rather than guessed at.
    ///
    /// Accepts a case still in `RevealPhase` as well as one that has
    /// collected every reveal: the chain can execute an outcome on a
    /// deadline that passed with seats unrevealed, and a node that refused
    /// to record that would be stuck showing a live case that has already
    /// paid out.
    pub fn apply_onchain_execution(
        &self,
        id: &DisputeId,
        signature: impl Into<String>,
        outcome: Option<Resolution>,
    ) -> Result<(), DisputeError> {
        let mut dispute = self.get(id).ok_or(DisputeError::DisputeNotFound)?;
        // Anything but an already-resolved case. The chain executes on
        // its own deadlines, not on this node's view of the phase: a
        // commit or reveal window can expire with seats unfilled and the
        // chain will decide anyway. A node that refused to record that
        // would go on showing a live case that has already paid out,
        // which is the divergence this whole change removes.
        if dispute.status == DisputeStatus::Resolved {
            return Err(DisputeError::InvalidStateTransition);
        }
        dispute.onchain_execution_signature = Some(signature.into());
        // The signature is always recorded — this node genuinely observed
        // that transaction confirm. The verdict is recorded only when the
        // node could actually read it from the case account. A node that
        // saw an execution land but could not read the outcome stays in
        // `AwaitingChainVerdict`, which is the truth: something happened
        // on chain and this node does not yet know what. Inventing a
        // verdict to fill the gap is the exact failure this change
        // removes.
        if let Some(outcome) = outcome {
            dispute.resolution = Some(outcome);
            dispute.status = DisputeStatus::Resolved;
        }
        self.put(&dispute);
        Ok(())
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match event.event_type.as_str() {
            protocol::EVENT_OPENED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_open(signed);
                }
            }
            protocol::EVENT_ARBITRATOR_JOINED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_arbitrator_join(signed);
                }
            }
            protocol::EVENT_VOTE_COMMITTED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_vote_commit(signed);
                }
            }
            protocol::EVENT_VOTE_REVEALED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_vote_reveal(signed);
                }
            }
            protocol::EVENT_MUTUAL_SETTLEMENT_AGREED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_mutual_settlement_agree(signed);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{
        ArbitratorJoin, DisputeOpen, MutualSettlementAgree, VoteCommit, VoteReveal,
    };
    use crate::record::Vote;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_reservations::ReservationId;
    use openfiat_settlement::SettlementId;
    use openfiat_settlement::events::SignedSettlementInitiate;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{Amount, Timestamp};

    fn setup() -> (
        Rc<SettlementRegistry<MemoryStore>>,
        DisputeRegistry<MemoryStore>,
        Keypair,
        Keypair,
        SettlementId,
    ) {
        let settlements = Rc::new(SettlementRegistry::new(MemoryStore::new()));
        let buyer = Keypair::generate();
        let seller = Keypair::generate();
        let initiate = openfiat_settlement::events::SettlementInitiate {
            id: SettlementId::new("settle-1"),
            reservation_id: ReservationId::new("res-1"),
            buyer: peer_id_from_public_key(&buyer.public_key()).unwrap(),
            buyer_public_key: buyer.public_key(),
            seller: peer_id_from_public_key(&seller.public_key()).unwrap(),
            seller_public_key: seller.public_key(),
            amount: Amount::new(2_000_000, 6),
            timestamp: Timestamp::now(),
        };
        let settlement_id = settlements
            .apply_initiate(SignedSettlementInitiate::sign(initiate, &buyer))
            .unwrap();
        let disputes = DisputeRegistry::new(MemoryStore::new(), Rc::clone(&settlements));
        (settlements, disputes, buyer, seller, settlement_id)
    }

    fn open_dispute(
        disputes: &DisputeRegistry<MemoryStore>,
        opener: &Keypair,
        settlement_id: &SettlementId,
    ) -> DisputeId {
        let open = DisputeOpen {
            id: DisputeId::new("dispute-1"),
            settlement_id: settlement_id.clone(),
            opener: peer_id_from_public_key(&opener.public_key()).unwrap(),
            opener_public_key: opener.public_key(),
            reason: "payment not received".to_string(),
            timestamp: Timestamp::now(),
        };
        disputes
            .apply_open(SignedDisputeOpen::sign(open, opener))
            .unwrap()
    }

    fn join(disputes: &DisputeRegistry<MemoryStore>, dispute_id: &DisputeId, arbitrator: &Keypair) {
        let join = ArbitratorJoin {
            dispute_id: dispute_id.clone(),
            arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(),
            arbitrator_public_key: arbitrator.public_key(),
            timestamp: Timestamp::now(),
        };
        disputes
            .apply_arbitrator_join(SignedArbitratorJoin::sign(join, arbitrator))
            .unwrap();
    }

    #[test]
    fn opening_by_a_non_party_is_rejected() {
        let (_settlements, disputes, _buyer, _seller, settlement_id) = setup();
        let stranger = Keypair::generate();
        let open = DisputeOpen {
            id: DisputeId::new("dispute-1"),
            settlement_id,
            opener: peer_id_from_public_key(&stranger.public_key()).unwrap(),
            opener_public_key: stranger.public_key(),
            reason: "not my trade".to_string(),
            timestamp: Timestamp::now(),
        };
        let result = disputes.apply_open(SignedDisputeOpen::sign(open, &stranger));
        assert_eq!(result, Err(DisputeError::NotAParty));
    }

    #[test]
    fn the_case_locks_once_the_required_number_of_arbitrators_join() {
        let (_settlements, disputes, buyer, _seller, settlement_id) = setup();
        let dispute_id = open_dispute(&disputes, &buyer, &settlement_id);
        let arbitrators: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();

        join(&disputes, &dispute_id, &arbitrators[0]);
        assert_eq!(
            disputes.get(&dispute_id).unwrap().status,
            DisputeStatus::Open
        );
        join(&disputes, &dispute_id, &arbitrators[1]);
        join(&disputes, &dispute_id, &arbitrators[2]);
        assert_eq!(
            disputes.get(&dispute_id).unwrap().status,
            DisputeStatus::CaseLocked
        );

        // A fourth arbitrator can no longer join.
        let fourth = Keypair::generate();
        let join_event = ArbitratorJoin {
            dispute_id: dispute_id.clone(),
            arbitrator: peer_id_from_public_key(&fourth.public_key()).unwrap(),
            arbitrator_public_key: fourth.public_key(),
            timestamp: Timestamp::now(),
        };
        let result =
            disputes.apply_arbitrator_join(SignedArbitratorJoin::sign(join_event, &fourth));
        assert_eq!(result, Err(DisputeError::InvalidStateTransition));
    }

    #[test]
    fn a_full_commit_reveal_round_collects_every_vote_and_decides_nothing() {
        let (_settlements, disputes, buyer, _seller, settlement_id) = setup();
        let dispute_id = open_dispute(&disputes, &buyer, &settlement_id);
        let arbitrators: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
        for arbitrator in &arbitrators {
            join(&disputes, &dispute_id, arbitrator);
        }

        let votes = [Vote::BuyerWins, Vote::BuyerWins, Vote::MerchantWins];
        let secrets = [[1u8; 32], [2u8; 32], [3u8; 32]];
        for ((arbitrator, vote), secret) in arbitrators.iter().zip(votes).zip(secrets) {
            let commitment = commitment::compute(vote, &secret);
            let commit = VoteCommit {
                dispute_id: dispute_id.clone(),
                arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(),
                commitment,
                timestamp: Timestamp::now(),
            };
            disputes
                .apply_vote_commit(SignedVoteCommit::sign(commit, arbitrator))
                .unwrap();
        }
        assert_eq!(
            disputes.get(&dispute_id).unwrap().status,
            DisputeStatus::RevealPhase
        );

        for ((arbitrator, vote), secret) in arbitrators.iter().zip(votes).zip(secrets) {
            let reveal = VoteReveal {
                dispute_id: dispute_id.clone(),
                arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(),
                vote,
                secret,
                timestamp: Timestamp::now(),
            };
            disputes
                .apply_vote_reveal(SignedVoteReveal::sign(reveal, arbitrator))
                .unwrap();
        }

        let dispute = disputes.get(&dispute_id).unwrap();
        assert_eq!(dispute.reveals.len(), 3, "every vote is recorded");
        assert_eq!(dispute.status, DisputeStatus::AwaitingChainVerdict);
        // Two of three revealed BuyerWins. This node used to call that
        // the answer, and the chain would then re-arbitrate the same case
        // stake-weighted, with a quorum floor, re-opening the round on a
        // tie rather than resolving it — so the two could and did reach
        // different verdicts about one dispute, with the interface
        // showing this one while the money followed the other.
        assert_eq!(
            dispute.resolution, None,
            "collecting votes is not the same as counting them, and only \
             one of the two is this node's job"
        );
    }

    #[test]
    fn a_resolved_case_cannot_be_resolved_again() {
        let (_settlements, disputes, buyer, _seller, settlement_id) = setup();
        let dispute_id = open_dispute(&disputes, &buyer, &settlement_id);
        // The chain may execute at any point before a case is resolved —
        // on an expired window with seats unfilled, for instance — so
        // there is no "too early". What cannot happen twice is a
        // resolution.
        disputes
            .apply_onchain_execution(&dispute_id, "sig-first", Some(Resolution::BuyerWins))
            .expect("the chain decides on its own schedule");
        let second = disputes.apply_onchain_execution(
            &dispute_id,
            "sig-second",
            Some(Resolution::MerchantWins),
        );
        assert_eq!(second, Err(DisputeError::InvalidStateTransition));
        assert_eq!(
            disputes.get(&dispute_id).unwrap().resolution,
            Some(Resolution::BuyerWins),
            "a second execution must not overwrite the decided outcome"
        );
    }

    #[test]
    fn onchain_execution_is_recorded_once_the_case_resolves() {
        let (_settlements, disputes, buyer, _seller, settlement_id) = setup();
        let dispute_id = open_dispute(&disputes, &buyer, &settlement_id);
        let arbitrators: Vec<Keypair> = (0..3).map(|_| Keypair::generate()).collect();
        for arbitrator in &arbitrators {
            join(&disputes, &dispute_id, arbitrator);
        }
        let votes = [Vote::BuyerWins, Vote::BuyerWins, Vote::MerchantWins];
        let secrets = [[1u8; 32], [2u8; 32], [3u8; 32]];
        for ((arbitrator, vote), secret) in arbitrators.iter().zip(votes).zip(secrets) {
            let commit = VoteCommit {
                dispute_id: dispute_id.clone(),
                arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(),
                commitment: commitment::compute(vote, &secret),
                timestamp: Timestamp::now(),
            };
            disputes
                .apply_vote_commit(SignedVoteCommit::sign(commit, arbitrator))
                .unwrap();
        }
        for ((arbitrator, vote), secret) in arbitrators.iter().zip(votes).zip(secrets) {
            let reveal = VoteReveal {
                dispute_id: dispute_id.clone(),
                arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(),
                vote,
                secret,
                timestamp: Timestamp::now(),
            };
            disputes
                .apply_vote_reveal(SignedVoteReveal::sign(reveal, arbitrator))
                .unwrap();
        }
        // Every reveal is in, and this node still has no verdict. It
        // collected the votes; the chain decides what they add up to.
        let awaiting = disputes.get(&dispute_id).unwrap();
        assert_eq!(awaiting.status, DisputeStatus::AwaitingChainVerdict);
        assert_eq!(
            awaiting.resolution, None,
            "the off-chain layer must not tally — the chain re-arbitrates \
             the same case under different rules and would have reached a \
             different answer here"
        );

        disputes
            .apply_onchain_execution(
                &dispute_id,
                "5xY...onchainSig",
                Some(Resolution::MerchantWins),
            )
            .unwrap();
        let resolved = disputes.get(&dispute_id).unwrap();
        assert_eq!(resolved.status, DisputeStatus::Resolved);
        // The chain's answer, not the two-to-one majority the reveals
        // above would have produced. That divergence is the entire point:
        // the chain weights by stake and this node cannot.
        assert_eq!(resolved.resolution, Some(Resolution::MerchantWins));
        assert_eq!(
            resolved.onchain_execution_signature,
            Some("5xY...onchainSig".to_string())
        );
    }

    #[test]
    fn an_execution_this_node_cannot_read_records_the_signature_and_no_verdict() {
        // A node that saw a transaction land but could not read the case
        // account. It knows something happened and not what — and saying
        // so is the honest answer, where filling the gap with a guess is
        // the failure this whole change removes.
        let (_settlements, disputes, buyer, _seller, settlement_id) = setup();
        let dispute_id = open_dispute(&disputes, &buyer, &settlement_id);

        disputes
            .apply_onchain_execution(&dispute_id, "sig-unreadable", None)
            .unwrap();
        let dispute = disputes.get(&dispute_id).unwrap();
        assert_eq!(
            dispute.onchain_execution_signature,
            Some("sig-unreadable".to_string())
        );
        assert_eq!(dispute.resolution, None);
        assert_ne!(dispute.status, DisputeStatus::Resolved);
    }

    #[test]
    fn a_reveal_not_matching_its_commitment_is_rejected() {
        let (_settlements, disputes, buyer, _seller, settlement_id) = setup();
        let dispute_id = open_dispute(&disputes, &buyer, &settlement_id);
        let arbitrator = Keypair::generate();
        let others: Vec<Keypair> = (0..2).map(|_| Keypair::generate()).collect();
        join(&disputes, &dispute_id, &arbitrator);
        for other in &others {
            join(&disputes, &dispute_id, other);
        }

        // All three commit, reaching the reveal phase.
        let commit = VoteCommit {
            dispute_id: dispute_id.clone(),
            arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(),
            commitment: commitment::compute(Vote::BuyerWins, &[1u8; 32]),
            timestamp: Timestamp::now(),
        };
        disputes
            .apply_vote_commit(SignedVoteCommit::sign(commit, &arbitrator))
            .unwrap();
        for other in &others {
            let commit = VoteCommit {
                dispute_id: dispute_id.clone(),
                arbitrator: peer_id_from_public_key(&other.public_key()).unwrap(),
                commitment: commitment::compute(Vote::MerchantWins, &[9u8; 32]),
                timestamp: Timestamp::now(),
            };
            disputes
                .apply_vote_commit(SignedVoteCommit::sign(commit, other))
                .unwrap();
        }
        assert_eq!(
            disputes.get(&dispute_id).unwrap().status,
            DisputeStatus::RevealPhase
        );

        // Reveals a *different* vote than committed.
        let reveal = VoteReveal {
            dispute_id,
            arbitrator: peer_id_from_public_key(&arbitrator.public_key()).unwrap(),
            vote: Vote::MerchantWins,
            secret: [1u8; 32],
            timestamp: Timestamp::now(),
        };
        let result = disputes.apply_vote_reveal(SignedVoteReveal::sign(reveal, &arbitrator));
        assert_eq!(result, Err(DisputeError::CommitmentMismatch));
    }

    #[test]
    fn mutual_settlement_resolves_once_both_parties_agree() {
        let (_settlements, disputes, buyer, seller, settlement_id) = setup();
        let dispute_id = open_dispute(&disputes, &buyer, &settlement_id);

        let buyer_agree = MutualSettlementAgree {
            dispute_id: dispute_id.clone(),
            party: peer_id_from_public_key(&buyer.public_key()).unwrap(),
            timestamp: Timestamp::now(),
        };
        disputes
            .apply_mutual_settlement_agree(SignedMutualSettlementAgree::sign(buyer_agree, &buyer))
            .unwrap();
        assert_eq!(
            disputes.get(&dispute_id).unwrap().status,
            DisputeStatus::Open
        );

        let seller_agree = MutualSettlementAgree {
            dispute_id: dispute_id.clone(),
            party: peer_id_from_public_key(&seller.public_key()).unwrap(),
            timestamp: Timestamp::now(),
        };
        disputes
            .apply_mutual_settlement_agree(SignedMutualSettlementAgree::sign(seller_agree, &seller))
            .unwrap();

        let dispute = disputes.get(&dispute_id).unwrap();
        assert_eq!(dispute.status, DisputeStatus::Resolved);
        assert_eq!(dispute.resolution, Some(Resolution::MutualSettlement));
    }
}
