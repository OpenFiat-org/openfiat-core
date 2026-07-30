//! What a stranger sees of a trade.
//!
//! # The hole this closes
//!
//! `getSettlements`, `getReservations` and `getDisputes` took no
//! parameters, required no authentication, and returned every record on
//! the network with both parties named and keyed. Together that is the
//! who-trades-with-whom graph — the exact thing `methods::counterparties`
//! builds a signing handshake to refuse, on the stated grounds that in a
//! P2P fiat market, knowing which merchant a wallet always returns to and
//! who a busy merchant's regulars are is a physical-safety question.
//!
//! One unauthenticated call reconstructed it. The gate was not weak, it
//! was walked around. And it was three methods rather than one: a
//! reservation names the buyer and the advertisement, and the
//! advertisement names the merchant, so the same edge is available one
//! step earlier — including for trades that never settled.
//!
//! # Why redaction rather than authentication
//!
//! An explorer showing settlement volume, states and timing is a
//! legitimate public view of a public network, and putting a signature in
//! front of it would break that while pushing anyone determined back to
//! raw gossip, which achieves nothing. What the explorer never needed is
//! *who*. So the public read keeps everything except identity, and the
//! parties to a trade read their own records in full through the
//! wallet-proof methods.
//!
//! # What this is honestly worth
//!
//! These records are gossiped to every node. Anyone running one reads
//! them all, and nothing here changes that. What is protected is the ease
//! of the query — the difference between `curl`-ing somebody else's
//! public access node and standing up a node to index the network. That
//! difference is most of what casual harvesting is made of, and it is the
//! same reasoning `counterparties` already accepted when it gated an
//! aggregate over records that are themselves replicated.
//!
//! # The rule for adding a field
//!
//! A field belongs in a public view only if it says something about the
//! *trade* rather than about the *people*. When in doubt it stays out:
//! adding one later is a release note, and removing one is a disclosure
//! that already happened.

use openfiat_disputes::{Dispute, DisputeId, DisputeStatus, Resolution};
use openfiat_reservations::{Reservation, ReservationId, ReservationState};
use openfiat_settlement::{Settlement, SettlementId, SettlementState};
use openfiat_types::{Amount, Timestamp};

/// A settlement with the parties removed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicSettlement {
    pub id: SettlementId,
    pub reservation_id: openfiat_reservations::ReservationId,
    pub amount: Amount,
    pub state: SettlementState,
    /// Kept: it names an on-chain transaction anyone can already read on
    /// Solana, and it is what makes a settlement independently checkable.
    pub escrow_release_signature: Option<String>,
    pub payment_submitted_at: Option<Timestamp>,
    pub merchant_responded_at: Option<Timestamp>,
    pub payment_discrepancy: Option<openfiat_settlement::PaymentDiscrepancy>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl From<Settlement> for PublicSettlement {
    fn from(s: Settlement) -> Self {
        // `payment_reference` is dropped along with the parties, and it
        // is arguably the worse of the two: it is free text a buyer puts
        // their own bank reference in, so it routinely carries a real
        // name or an account number. Nothing outside the trade has any
        // business reading it.
        Self {
            id: s.id,
            reservation_id: s.reservation_id,
            amount: s.amount,
            state: s.state,
            escrow_release_signature: s.escrow_release_signature,
            payment_submitted_at: s.payment_submitted_at,
            merchant_responded_at: s.merchant_responded_at,
            payment_discrepancy: s.payment_discrepancy,
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

/// A reservation with the requester removed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicReservation {
    pub id: ReservationId,
    /// Kept deliberately. An advertisement is a public offer and already
    /// carries its merchant's peer id on every order-book row, so this
    /// discloses one end of an edge that was never private. What it does
    /// not disclose is the other end, which is what makes it an edge.
    pub advertisement_id: openfiat_advertisements::AdvertisementId,
    pub amount: Amount,
    /// The price this reservation was struck at, and the oracle mid
    /// behind it.
    ///
    /// Kept, and it was an oversight that the first version of this
    /// dropped them. The rule this module states is that a field belongs
    /// in a public view if it says something about the *trade* rather
    /// than the *people*, and a price is the most trade-like fact there
    /// is — it is what an explorer showing the market needs, and it
    /// discloses nothing about who agreed to it. Caught by an SDK author
    /// reading the rule and noticing the code did not follow it.
    pub agreed_price: Amount,
    pub agreed_mid: Option<f64>,
    pub state: ReservationState,
    pub requested_at: Timestamp,
    pub updated_at: Timestamp,
    pub expires_at: Timestamp,
}

impl From<Reservation> for PublicReservation {
    fn from(r: Reservation) -> Self {
        Self {
            id: r.id,
            advertisement_id: r.advertisement_id,
            amount: r.amount,
            agreed_price: r.agreed_price,
            agreed_mid: r.agreed_mid,
            state: r.state,
            requested_at: r.requested_at,
            updated_at: r.updated_at,
            expires_at: r.expires_at,
        }
    }
}

/// A dispute with the parties, the arbitrators and their votes removed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicDispute {
    pub id: DisputeId,
    pub settlement_id: SettlementId,
    pub status: DisputeStatus,
    pub required_arbitrators: u8,
    /// How many seats are filled, without saying by whom.
    pub arbitrators_seated: usize,
    /// How many have committed and how many have revealed — enough for an
    /// explorer to show a case progressing, with nobody's vote attached
    /// to their name.
    pub commitments: usize,
    pub reveals: usize,
    /// The outcome, which is the point of the case and is enforced on
    /// chain where anyone can read it anyway.
    pub resolution: Option<Resolution>,
    pub onchain_execution_signature: Option<String>,
    pub opened_at: Timestamp,
    pub updated_at: Timestamp,
}

impl From<Dispute> for PublicDispute {
    fn from(d: Dispute) -> Self {
        // Three separate reasons for what is dropped here.
        //
        // `buyer`/`seller`/`opener` and their keys: the trade graph, as
        // above — and a dispute is the case where knowing who fell out
        // with whom is most obviously worth misusing.
        //
        // `reason` is free text written by the party opening the case.
        // It describes a real disagreement about real money and it names
        // people, banks and references as a matter of course.
        //
        // `arbitrators`, `arbitrator_keys`, `commitments` and `reveals`:
        // an arbitrator is a registered provider and their identity is
        // not itself a secret, but *which arbitrator drew which case, and
        // how they voted* is exactly the pairing that makes pressuring
        // one worthwhile. Counts survive so a case can be seen to be
        // progressing; the pairing does not. The mutual-settlement flags
        // go with them — "the seller has agreed and the buyer has not" is
        // a negotiating position, and publishing it to onlookers changes
        // a negotiation between two people.
        Self {
            id: d.id,
            settlement_id: d.settlement_id,
            status: d.status,
            required_arbitrators: d.required_arbitrators,
            arbitrators_seated: d.arbitrators.len(),
            commitments: d.commitments.len(),
            reveals: d.reveals.len(),
            resolution: d.resolution,
            onchain_execution_signature: d.onchain_execution_signature,
            opened_at: d.opened_at,
            updated_at: d.updated_at,
        }
    }
}

/// A trade with both parties removed.
///
/// `getTrades` is a join over a reservation and its settlement, and it
/// was the way around everything above: it returned both records whole,
/// unauthenticated and unparameterised, so redacting the three underlying
/// reads left the graph available one method along. Found by someone
/// reading the API reference, which is exactly who would find it.
///
/// A join of two redacted records is redacted; there is nothing extra to
/// remove, and composing it from the same `From` impls means a field
/// added to either half cannot appear here without appearing there.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicTrade {
    pub reservation: PublicReservation,
    pub settlement: Option<PublicSettlement>,
    /// The derived status, which is what a trade view is for and says
    /// nothing about who is party to it.
    pub status: openfiat_trade::TradeStatus,
}

impl From<openfiat_trade::Trade> for PublicTrade {
    fn from(trade: openfiat_trade::Trade) -> Self {
        Self {
            status: trade.status(),
            reservation: trade.reservation.into(),
            settlement: trade.settlement.map(PublicSettlement::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_types::PeerId;

    fn peer(seed: u8) -> (PeerId, openfiat_types::PublicKey) {
        let keypair = Keypair::from_seed([seed; 32]);
        (
            peer_id_from_public_key(&keypair.public_key()).unwrap(),
            keypair.public_key(),
        )
    }

    fn settlement() -> Settlement {
        let (buyer, buyer_key) = peer(1);
        let (seller, seller_key) = peer(2);
        Settlement {
            id: SettlementId::new("s-1"),
            reservation_id: ReservationId::new("r-1"),
            buyer,
            buyer_public_key: buyer_key,
            seller,
            seller_public_key: seller_key,
            amount: Amount::new(1_000_000, 6),
            state: SettlementState::Completed,
            payment_reference: Some("Jane Doe, Equity 0123456789".into()),
            escrow_release_signature: Some("sig".into()),
            payment_submitted_at: Some(Timestamp::from_millis(10)),
            merchant_responded_at: Some(Timestamp::from_millis(20)),
            payment_discrepancy: None,
            created_at: Timestamp::from_millis(1),
            updated_at: Timestamp::from_millis(30),
        }
    }

    /// The assertion is on the serialized form, not the struct, because
    /// the struct's fields are the thing under test: a field added to
    /// `PublicSettlement` later would compile fine and quietly widen what
    /// a stranger sees.
    #[test]
    fn a_public_settlement_names_neither_party_nor_their_payment_reference() {
        let json = serde_json::to_string(&PublicSettlement::from(settlement())).unwrap();
        for leaked in ["buyer", "seller", "public_key", "Jane Doe", "0123456789"] {
            assert!(
                !json.contains(leaked),
                "{leaked:?} must not reach an unauthenticated reader: {json}"
            );
        }
    }

    #[test]
    fn a_public_settlement_still_says_what_happened_and_when() {
        // The redaction has to leave an explorer something real, or the
        // honest response is to remove the endpoint rather than serve a
        // hollow one.
        let json = serde_json::to_string(&PublicSettlement::from(settlement())).unwrap();
        for kept in ["s-1", "r-1", "Completed", "sig"] {
            assert!(json.contains(kept), "{kept:?} is not identity: {json}");
        }
    }

    #[test]
    fn a_public_reservation_keeps_the_advertisement_and_drops_the_requester() {
        let (requester, requester_key) = peer(3);
        let reservation = Reservation {
            id: ReservationId::new("r-9"),
            advertisement_id: openfiat_advertisements::AdvertisementId::new("ad-7"),
            requester,
            requester_public_key: requester_key,
            amount: Amount::new(50, 2),
            agreed_price: Amount::new(12_950, 2),
            agreed_mid: None,
            state: ReservationState::EscrowLocked,
            requested_at: Timestamp::from_millis(1),
            updated_at: Timestamp::from_millis(2),
            expires_at: Timestamp::from_millis(3),
        };
        let json = serde_json::to_string(&PublicReservation::from(reservation)).unwrap();
        assert!(json.contains("ad-7"), "a public offer is already public");
        assert!(
            !json.contains("requester"),
            "the other end of the edge is not"
        );
    }

    #[test]
    fn a_public_dispute_counts_the_arbitrators_without_naming_them() {
        let (buyer, buyer_key) = peer(4);
        let (seller, seller_key) = peer(5);
        let (arbitrator, arbitrator_key) = peer(6);
        let dispute = Dispute {
            id: DisputeId::new("d-1"),
            settlement_id: SettlementId::new("s-1"),
            buyer,
            buyer_public_key: buyer_key,
            seller: seller.clone(),
            seller_public_key: seller_key,
            opener: seller,
            reason: "He never sent the money, his name is Bob and his bank is X".into(),
            status: DisputeStatus::Open,
            required_arbitrators: 3,
            arbitrators: vec![arbitrator.clone()],
            arbitrator_keys: vec![(arbitrator, arbitrator_key)],
            commitments: Vec::new(),
            reveals: Vec::new(),
            resolution: None,
            buyer_agreed_mutual_settlement: true,
            seller_agreed_mutual_settlement: false,
            onchain_execution_signature: None,
            opened_at: Timestamp::from_millis(1),
            updated_at: Timestamp::from_millis(2),
        };

        let public = PublicDispute::from(dispute);
        assert_eq!(public.arbitrators_seated, 1);
        assert_eq!(public.required_arbitrators, 3);

        let json = serde_json::to_string(&public).unwrap();
        for leaked in ["Bob", "reason", "opener", "arbitrator_keys", "mutual"] {
            assert!(
                !json.contains(leaked),
                "{leaked:?} must not reach an unauthenticated reader: {json}"
            );
        }
        assert!(json.contains("d-1") && json.contains("Open"));
    }
}
