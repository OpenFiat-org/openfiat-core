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
    ArbitratorCommitment, ArbitratorReveal, Dispute, DisputeId, DisputeStatus, Resolution, Vote,
};
use openfiat_crypto::verify;
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
        let bytes = wire::to_bytes(&signed.commit).map_err(|_| DisputeError::MalformedDispute)?;
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
        let bytes = wire::to_bytes(&signed.reveal).map_err(|_| DisputeError::MalformedDispute)?;
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
            dispute.resolution = Some(consensus(&dispute.reveals));
            dispute.status = DisputeStatus::Resolved;
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

        let bytes = wire::to_bytes(&signed.agree).map_err(|_| DisputeError::MalformedDispute)?;
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

/// Majority vote among valid reveals; a genuine tie resolves to
/// `Invalid` (inconclusive) as a safe, deterministic fallback rather than
/// picking an arbitrary winner.
fn consensus(reveals: &[ArbitratorReveal]) -> Resolution {
    let (mut buyer, mut merchant, mut invalid) = (0u32, 0u32, 0u32);
    for reveal in reveals {
        match reveal.vote {
            Vote::BuyerWins => buyer += 1,
            Vote::MerchantWins => merchant += 1,
            Vote::Invalid => invalid += 1,
        }
    }
    if buyer > merchant && buyer > invalid {
        Resolution::BuyerWins
    } else if merchant > buyer && merchant > invalid {
        Resolution::MerchantWins
    } else {
        Resolution::Invalid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{
        ArbitratorJoin, DisputeOpen, MutualSettlementAgree, VoteCommit, VoteReveal,
    };
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
    fn a_full_commit_reveal_round_reaches_the_majority_resolution() {
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
        assert_eq!(dispute.status, DisputeStatus::Resolved);
        assert_eq!(dispute.resolution, Some(Resolution::BuyerWins));
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
