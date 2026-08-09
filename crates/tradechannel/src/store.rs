//! The replicated trade-channel index.
//!
//! This registry holds ciphertext and metadata, and nothing else. It has
//! no channel key, cannot obtain one, and has no code path that would
//! know what to do with one — which is what makes "sealed" a structural
//! statement about this node rather than a promise about its read
//! handlers.
//!
//! What it *does* enforce is who may write and who may be given the
//! ability to read: both derived from records this node already verified
//! (the settlement's parties, the arbitrators on a dispute over it),
//! never from anything the event itself asserts.

use crate::error::TradeChannelError;
use crate::events::{SignedTradeChannelEntryPost, SignedTradeChannelKeyGrant};
use crate::key::MAX_ENTRY_CIPHERTEXT;
use crate::protocol;
use crate::record::{ChannelEntry, GrantRole, KeyGrant, TradeChannel};
use openfiat_crypto::verify;
use openfiat_disputes::DisputeRegistry;
use openfiat_serialization::wire;
use openfiat_settlement::{Settlement, SettlementId, SettlementRegistry};
use openfiat_storage::KvStore;
use openfiat_types::{EventEnvelope, PeerId, PublicKey};
use std::rc::Rc;

pub const GRANTS_COLUMN_FAMILY: &str = "trade_channel_grants";
pub const ENTRIES_COLUMN_FAMILY: &str = "trade_channel_entries";

/// One node's view of every trade channel it has replicated.
///
/// Holds read handles to the settlement and dispute registries for the
/// same reason `DisputeRegistry` holds one to settlements: the facts that
/// authorize a write live in those records and must be read from them
/// rather than re-declared and separately trusted.
pub struct TradeChannelRegistry<S> {
    store: S,
    settlements: Rc<SettlementRegistry<S>>,
    /// Read-only, and only ever consulted to answer one question: has
    /// `recipient` joined a dispute over this settlement? That is the
    /// whole of this crate's dependency on arbitration.
    disputes: Rc<DisputeRegistry<S>>,
}

impl<S: KvStore> TradeChannelRegistry<S> {
    pub fn new(
        store: S,
        settlements: Rc<SettlementRegistry<S>>,
        disputes: Rc<DisputeRegistry<S>>,
    ) -> Self {
        Self {
            store,
            settlements,
            disputes,
        }
    }

    /// The whole channel for one settlement, assembled and deterministically
    /// ordered.
    ///
    /// Returns an empty channel rather than `None` for a settlement with
    /// nothing written to it: "this trade has no conversation yet" is a
    /// real, displayable answer, and a caller forced to distinguish it
    /// from an error would invent the empty one itself.
    pub fn channel(&self, settlement_id: &SettlementId) -> TradeChannel {
        let prefix = channel_prefix(settlement_id);

        let mut grants: Vec<KeyGrant> = self
            .store
            .iter_prefix(GRANTS_COLUMN_FAMILY, &prefix)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect();
        // Key order already groups by recipient; sorting explicitly keeps
        // the rendered list stable even if the key layout ever changes.
        grants.sort_by(|a: &KeyGrant, b: &KeyGrant| {
            a.recipient
                .as_bytes()
                .cmp(b.recipient.as_bytes())
                .then_with(|| a.granter.as_bytes().cmp(b.granter.as_bytes()))
        });

        let mut entries: Vec<ChannelEntry> = self
            .store
            .iter_prefix(ENTRIES_COLUMN_FAMILY, &prefix)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(_, value)| wire::from_bytes(&value).ok())
            .collect();
        // By claimed time, because that is the only ordering the two
        // parties can agree on without a shared clock; then by author and
        // sequence so a tie — two clients on the same millisecond, or one
        // backdating — still renders identically on every node.
        entries.sort_by(|a: &ChannelEntry, b: &ChannelEntry| {
            a.posted_at
                .as_millis()
                .cmp(&b.posted_at.as_millis())
                .then_with(|| a.author.as_bytes().cmp(b.author.as_bytes()))
                .then_with(|| a.sequence.cmp(&b.sequence))
        });

        TradeChannel {
            settlement_id: settlement_id.clone(),
            grants,
            entries,
        }
    }

    /// A party handing the channel key to a permitted reader.
    ///
    /// # Why the readership is constrained at all
    ///
    /// A party who wanted to leak their own trade could send the key over
    /// any messaging app; refusing a grant here would not stop them. What
    /// the constraint buys is that the *replicated* record of who can
    /// read a channel is complete and checkable: [`TradeChannel::readers`]
    /// is then an honest answer to "who has been let in", which in turn
    /// makes a party's refusal to disclose to an arbitrator visible to
    /// everyone rather than deniable.
    ///
    /// # Why an arbitrator cannot be granted before they join
    ///
    /// Because until they join there is nothing this node can check. The
    /// dispute record is what makes "this peer is an arbitrator on this
    /// trade" a fact rather than the granter's word, and it is also the
    /// honest resolution of the collision at the heart of this feature:
    /// an arbitrator genuinely is not known when the messages are
    /// written, so they genuinely cannot be a recipient until they are.
    ///
    /// # What one party can do to the other's half of the conversation
    ///
    /// Disclose it. A party who opens a dispute and grants the key to an
    /// arbitrator discloses everything the counterparty wrote as well as
    /// everything they wrote themselves, and there is no consent step.
    ///
    /// That is not a hole so much as an unavoidable property, because it
    /// grants no capability that was not already held: a party can read
    /// every message in their own channel and could photograph the screen.
    /// The same answer covers the sharper version — a party arranging for
    /// an accomplice to join the dispute as an arbitrator and granting to
    /// them. It buys the accomplice nothing the party could not have
    /// forwarded by hand, and unlike forwarding it leaves a signed,
    /// replicated record naming exactly who was let in.
    ///
    /// A grant also survives the dispute that permitted it: the
    /// arbitrator stays on the case record after it resolves, so the key
    /// can still be granted afterwards. Intentional — an appeal or an
    /// audit of a decided case needs the same evidence the decision was
    /// made on.
    pub fn apply_key_grant(
        &self,
        signed: SignedTradeChannelKeyGrant,
    ) -> Result<(), TradeChannelError> {
        let settlement = self.settlement(&signed.grant.settlement_id)?;
        let granter_key = party_key(&settlement, &signed.grant.granter)?;
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::TRADE_CHANNEL_KEY_GRANT,
            &signed.grant,
        )
        .map_err(|_| TradeChannelError::MalformedEntry)?;
        verify(&granter_key, &bytes, &signed.signature)
            .map_err(|_| TradeChannelError::InvalidSignature)?;

        let role = self.role_of(&settlement, &signed.grant.recipient)?;
        let grant = KeyGrant {
            settlement_id: signed.grant.settlement_id,
            granter: signed.grant.granter,
            recipient: signed.grant.recipient,
            role,
            key_id: signed.grant.key_id,
            sealed_key: signed.grant.sealed_key,
            granted_at: signed.grant.timestamp,
        };
        // Keyed by (recipient, granter), so both parties can grant the
        // same arbitrator independently and neither can overwrite the
        // other's grant with a key that does not open the channel.
        let key = grant_key(&grant.settlement_id, &grant.recipient, &grant.granter);
        if let Ok(bytes) = wire::to_bytes(&grant) {
            let _ = self.store.put(GRANTS_COLUMN_FAMILY, &key, &bytes);
        }
        Ok(())
    }

    /// A party writing one entry — payment details or a message — into a
    /// channel they are in.
    ///
    /// The payload is opaque here. This node checks that its author is a
    /// party, that they signed it, that it fits, and that they have not
    /// already written something else at that sequence number. It cannot
    /// check that the ciphertext is well-formed, or addressed to anyone,
    /// or even that it is a ciphertext at all — and a node that pretended
    /// otherwise would be claiming a guarantee it has no key to verify.
    pub fn apply_entry(
        &self,
        signed: SignedTradeChannelEntryPost,
    ) -> Result<(), TradeChannelError> {
        let settlement = self.settlement(&signed.post.settlement_id)?;
        let author_key = party_key(&settlement, &signed.post.author)?;
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::TRADE_CHANNEL_ENTRY_POST,
            &signed.post,
        )
        .map_err(|_| TradeChannelError::MalformedEntry)?;
        verify(&author_key, &bytes, &signed.signature)
            .map_err(|_| TradeChannelError::InvalidSignature)?;
        if signed.post.payload.ciphertext.len() > MAX_ENTRY_CIPHERTEXT {
            return Err(TradeChannelError::EntryTooLarge);
        }

        let entry = ChannelEntry {
            settlement_id: signed.post.settlement_id,
            author: signed.post.author,
            sequence: signed.post.sequence,
            kind: signed.post.kind,
            payload: signed.post.payload,
            posted_at: signed.post.timestamp,
        };
        let key = entry_key(&entry.settlement_id, &entry.author, entry.sequence);
        // Gossip delivers the same event more than once as a matter of
        // course, so a byte-identical repost is a no-op rather than an
        // error. A *different* entry at a taken sequence number is an
        // author trying to rewrite what they already signed, and is
        // refused — the first version stands, and it stands everywhere,
        // because every node applies this same rule to the same events.
        if let Ok(Some(existing)) = self.store.get(ENTRIES_COLUMN_FAMILY, &key)
            && let Ok(existing) = wire::from_bytes::<ChannelEntry>(&existing)
        {
            return if existing == entry {
                Ok(())
            } else {
                Err(TradeChannelError::SequenceReused)
            };
        }
        if let Ok(bytes) = wire::to_bytes(&entry) {
            let _ = self.store.put(ENTRIES_COLUMN_FAMILY, &key, &bytes);
        }
        Ok(())
    }

    pub fn apply_event(&self, event: &EventEnvelope) {
        if event.ofs_spec != protocol::OFS_SPEC {
            return;
        }
        match event.event_type.as_str() {
            protocol::EVENT_KEY_GRANTED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_key_grant(signed);
                }
            }
            protocol::EVENT_ENTRY_POSTED => {
                if let Ok(signed) = wire::from_bytes(&event.payload) {
                    let _ = self.apply_entry(signed);
                }
            }
            _ => {}
        }
    }

    fn settlement(&self, id: &SettlementId) -> Result<Settlement, TradeChannelError> {
        self.settlements
            .get(id)
            .ok_or(TradeChannelError::SettlementNotFound)
    }

    /// Why `recipient` is allowed to read this channel, or an error if
    /// they are not.
    fn role_of(
        &self,
        settlement: &Settlement,
        recipient: &PeerId,
    ) -> Result<GrantRole, TradeChannelError> {
        if recipient == &settlement.buyer || recipient == &settlement.seller {
            return Ok(GrantRole::Party);
        }
        let is_arbitrator = self.disputes.all().into_iter().any(|dispute| {
            dispute.settlement_id == settlement.id && dispute.arbitrators.contains(recipient)
        });
        if is_arbitrator {
            Ok(GrantRole::Arbitrator)
        } else {
            Err(TradeChannelError::RecipientNotPermitted)
        }
    }
}

/// The settlement's recorded public key for `peer`, or `NotAParty`.
///
/// Read from the already-verified settlement rather than taken from the
/// event, so a signer cannot present a key of their choosing alongside a
/// peer id of their choosing.
fn party_key(settlement: &Settlement, peer: &PeerId) -> Result<PublicKey, TradeChannelError> {
    if peer == &settlement.buyer {
        Ok(settlement.buyer_public_key)
    } else if peer == &settlement.seller {
        Ok(settlement.seller_public_key)
    } else {
        Err(TradeChannelError::NotAParty)
    }
}

/// The shared key prefix of everything belonging to one channel.
///
/// Length-prefixed rather than separator-joined because a `SettlementId`
/// is an arbitrary string: with a separator byte, an id containing that
/// byte could be spelled two ways and a prefix scan for `settle-1` would
/// pick up `settle-1x`'s rows.
fn channel_prefix(settlement_id: &SettlementId) -> Vec<u8> {
    length_prefixed(settlement_id.as_str().as_bytes())
}

fn entry_key(settlement_id: &SettlementId, author: &PeerId, sequence: u64) -> Vec<u8> {
    let mut key = channel_prefix(settlement_id);
    key.extend_from_slice(&length_prefixed(author.as_bytes()));
    key.extend_from_slice(&sequence.to_be_bytes());
    key
}

fn grant_key(settlement_id: &SettlementId, recipient: &PeerId, granter: &PeerId) -> Vec<u8> {
    let mut key = channel_prefix(settlement_id);
    key.extend_from_slice(&length_prefixed(recipient.as_bytes()));
    key.extend_from_slice(granter.as_bytes());
    key
}

/// Big-endian length so that ordering by key orders by the field itself,
/// which is what makes a sequence-number suffix scan in order.
fn length_prefixed(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + field.len());
    out.extend_from_slice(&(field.len() as u32).to_be_bytes());
    out.extend_from_slice(field);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{TradeChannelEntryPost, TradeChannelKeyGrant};
    use crate::key::{ChannelKey, open_entry, seal_entry};
    use crate::record::EntryKind;
    use openfiat_advertisements::AdvertisementRegistry;
    use openfiat_crypto::{Keypair, seal};
    use openfiat_disputes::DisputeId;
    use openfiat_disputes::events::{
        ArbitratorJoin, DisputeOpen, SignedArbitratorJoin, SignedDisputeOpen,
    };
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_reservations::{ReservationId, ReservationRegistry};
    use openfiat_settlement::events::{SettlementInitiate, SignedSettlementInitiate};
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{Amount, Timestamp};

    struct Fixture {
        settlements: Rc<SettlementRegistry<MemoryStore>>,
        disputes: Rc<DisputeRegistry<MemoryStore>>,
        channels: TradeChannelRegistry<MemoryStore>,
        buyer: Keypair,
        seller: Keypair,
        settlement_id: SettlementId,
    }

    /// The reservation index a settlement registry requires (OFS-2300
    /// §5a). Nothing in this crate's tests creates a reservation — the
    /// subject here is who may write to a channel — so the
    /// reservation-side transitions find nothing and change nothing.
    fn empty_reservations() -> Rc<ReservationRegistry<MemoryStore>> {
        Rc::new(ReservationRegistry::new(
            MemoryStore::new(),
            Rc::new(AdvertisementRegistry::new(MemoryStore::new())),
        ))
    }

    impl Fixture {
        fn new() -> Self {
            let settlements = Rc::new(SettlementRegistry::new(
                MemoryStore::new(),
                empty_reservations(),
            ));
            let disputes = Rc::new(DisputeRegistry::new(
                MemoryStore::new(),
                Rc::clone(&settlements),
            ));
            let channels = TradeChannelRegistry::new(
                MemoryStore::new(),
                Rc::clone(&settlements),
                Rc::clone(&disputes),
            );
            let buyer = Keypair::generate();
            let seller = Keypair::generate();
            let settlement_id = SettlementId::new("settle-1");
            settlements
                .apply_initiate(SignedSettlementInitiate::sign(
                    SettlementInitiate {
                        id: settlement_id.clone(),
                        reservation_id: ReservationId::new("res-1"),
                        buyer: peer(&buyer),
                        buyer_public_key: buyer.public_key(),
                        seller: peer(&seller),
                        seller_public_key: seller.public_key(),
                        amount: Amount::new(2_000_000, 6),
                        timestamp: Timestamp::now(),
                    },
                    &buyer,
                ))
                .expect("a fresh settlement is always accepted");
            Self {
                settlements,
                disputes,
                channels,
                buyer,
                seller,
                settlement_id,
            }
        }

        /// Puts `arbitrator` on a real dispute over this settlement,
        /// through the same signed events arbitration actually uses, so
        /// the grant check below is exercised against reachable state.
        fn open_dispute_with(&self, arbitrator: &Keypair) {
            self.disputes
                .apply_open(SignedDisputeOpen::sign(
                    DisputeOpen {
                        id: DisputeId::new("dispute-1"),
                        settlement_id: self.settlement_id.clone(),
                        opener: peer(&self.buyer),
                        opener_public_key: self.buyer.public_key(),
                        reason: "funds never arrived".to_string(),
                        timestamp: Timestamp::now(),
                    },
                    &self.buyer,
                ))
                .expect("a party may open a dispute");
            self.disputes
                .apply_arbitrator_join(SignedArbitratorJoin::sign(
                    ArbitratorJoin {
                        dispute_id: DisputeId::new("dispute-1"),
                        arbitrator: peer(arbitrator),
                        arbitrator_public_key: arbitrator.public_key(),
                        timestamp: Timestamp::now(),
                    },
                    arbitrator,
                ))
                .expect("an open case accepts arbitrators");
        }

        fn grant(
            &self,
            granter: &Keypair,
            recipient: &Keypair,
            key: &ChannelKey,
        ) -> Result<(), TradeChannelError> {
            self.channels
                .apply_key_grant(SignedTradeChannelKeyGrant::sign(
                    TradeChannelKeyGrant {
                        settlement_id: self.settlement_id.clone(),
                        granter: peer(granter),
                        recipient: peer(recipient),
                        key_id: key.id(),
                        sealed_key: seal(&recipient.public_key(), key.expose()).unwrap(),
                        timestamp: Timestamp::now(),
                    },
                    granter,
                ))
        }

        fn post(
            &self,
            author: &Keypair,
            sequence: u64,
            kind: EntryKind,
            key: &ChannelKey,
            plaintext: &[u8],
        ) -> Result<(), TradeChannelError> {
            let author_id = peer(author);
            let binding = crate::key::EntryBinding {
                settlement_id: &self.settlement_id,
                author: &author_id,
                sequence,
                kind: kind.name(),
            };
            let payload = seal_entry(key, &binding, plaintext)?;
            self.channels.apply_entry(SignedTradeChannelEntryPost::sign(
                TradeChannelEntryPost {
                    settlement_id: self.settlement_id.clone(),
                    author: author_id.clone(),
                    sequence,
                    kind,
                    payload,
                    timestamp: Timestamp::now(),
                },
                author,
            ))
        }
    }

    fn peer(keypair: &Keypair) -> PeerId {
        peer_id_from_public_key(&keypair.public_key()).unwrap()
    }

    #[test]
    fn a_seller_discloses_payment_details_that_only_the_buyer_can_read() {
        let fixture = Fixture::new();
        let key = ChannelKey::generate();
        fixture
            .grant(&fixture.seller, &fixture.buyer, &key)
            .unwrap();
        fixture
            .post(
                &fixture.seller,
                0,
                EntryKind::PaymentDetails,
                &key,
                b"Equity Bank 0110123456789, R. Kimani",
            )
            .unwrap();

        let channel = fixture.channels.channel(&fixture.settlement_id);
        let details = channel.payment_details();
        assert_eq!(details.len(), 1);

        // The buyer opens their grant with their own private key, and the
        // key inside opens the entry.
        let grant = channel.grants_for(&peer(&fixture.buyer))[0];
        let recovered = ChannelKey::from_bytes(
            openfiat_crypto::open(&fixture.buyer, &grant.sealed_key)
                .expect("the grant is sealed to the buyer")
                .try_into()
                .expect("a channel key is 32 bytes"),
        );
        assert_eq!(recovered.id(), grant.key_id);
        assert_eq!(
            open_entry(&recovered, &details[0].binding(), &details[0].payload).unwrap(),
            b"Equity Bank 0110123456789, R. Kimani"
        );
    }

    /// The adversarial case this whole crate exists for. A third node
    /// replicates the entry — it is gossiped to everyone — and holds it
    /// forever, and can do nothing with it.
    #[test]
    fn a_third_party_replicating_the_channel_cannot_read_a_single_entry() {
        let fixture = Fixture::new();
        let outsider = Keypair::generate();
        let key = ChannelKey::generate();
        fixture
            .grant(&fixture.seller, &fixture.buyer, &key)
            .unwrap();
        fixture
            .post(
                &fixture.seller,
                0,
                EntryKind::PaymentDetails,
                &key,
                b"Equity Bank 0110123456789",
            )
            .unwrap();

        let channel = fixture.channels.channel(&fixture.settlement_id);
        assert!(!channel.is_reader(&peer(&outsider)));
        assert!(channel.grants_for(&peer(&outsider)).is_empty());

        // Every sealed key on the record refuses to open for them...
        for grant in &channel.grants {
            assert!(
                openfiat_crypto::open(&outsider, &grant.sealed_key).is_err(),
                "a grant addressed to a party must not open for anyone else"
            );
        }
        // ...and the ciphertext they *do* hold is inert. Guessing the key
        // is the only path left, which is the intended one.
        let entry = &channel.entries[0];
        assert_eq!(
            open_entry(&ChannelKey::generate(), &entry.binding(), &entry.payload),
            Err(TradeChannelError::PayloadDidNotOpen)
        );
        assert!(
            !entry
                .payload
                .ciphertext
                .windows(13)
                .any(|window| window == b"0110123456789"),
            "the stored bytes must not contain the account number"
        );
    }

    #[test]
    fn a_stranger_cannot_write_into_someone_elses_channel() {
        let fixture = Fixture::new();
        let stranger = Keypair::generate();
        let key = ChannelKey::generate();
        assert_eq!(
            fixture.post(&stranger, 0, EntryKind::Message, &key, b"hello"),
            Err(TradeChannelError::NotAParty)
        );
        assert!(
            fixture
                .channels
                .channel(&fixture.settlement_id)
                .entries
                .is_empty()
        );
    }

    #[test]
    fn a_stranger_cannot_grant_themselves_the_key() {
        let fixture = Fixture::new();
        let stranger = Keypair::generate();
        let key = ChannelKey::generate();
        assert_eq!(
            fixture.grant(&stranger, &stranger, &key),
            Err(TradeChannelError::NotAParty)
        );
    }

    /// A party cannot widen the readership to whoever they like *through
    /// the protocol*. They could always leak the key out of band — the
    /// point is that the replicated record of who was let in stays
    /// complete, so `readers()` is an honest answer.
    #[test]
    fn a_party_cannot_grant_the_key_to_an_uninvolved_peer() {
        let fixture = Fixture::new();
        let bystander = Keypair::generate();
        let key = ChannelKey::generate();
        assert_eq!(
            fixture.grant(&fixture.seller, &bystander, &key),
            Err(TradeChannelError::RecipientNotPermitted)
        );
    }

    /// The honest answer to "an arbitrator is not known when the messages
    /// are written": until they join a dispute, this node has nothing to
    /// check, so there is nobody to grant to.
    #[test]
    fn an_arbitrator_cannot_be_granted_the_key_before_joining_the_dispute() {
        let fixture = Fixture::new();
        let arbitrator = Keypair::generate();
        let key = ChannelKey::generate();
        assert_eq!(
            fixture.grant(&fixture.buyer, &arbitrator, &key),
            Err(TradeChannelError::RecipientNotPermitted)
        );
    }

    /// The resolution of the collision at the heart of #101: the messages
    /// were written before this arbitrator existed, and they read the
    /// original ciphertexts — not a re-encryption the disclosing party
    /// produced after the argument started.
    #[test]
    fn an_arbitrator_who_joined_a_dispute_can_be_granted_the_key_and_reads_the_original_history() {
        let fixture = Fixture::new();
        let key = ChannelKey::generate();
        fixture
            .grant(&fixture.seller, &fixture.buyer, &key)
            .unwrap();
        fixture
            .post(
                &fixture.seller,
                0,
                EntryKind::PaymentDetails,
                &key,
                b"Equity Bank 0110123456789",
            )
            .unwrap();
        fixture
            .post(&fixture.buyer, 0, EntryKind::Message, &key, b"sent it")
            .unwrap();
        let before_dispute = fixture.channels.channel(&fixture.settlement_id);

        let arbitrator = Keypair::generate();
        fixture.open_dispute_with(&arbitrator);
        fixture
            .grant(&fixture.buyer, &arbitrator, &key)
            .expect("a joined arbitrator is a permitted recipient");

        let channel = fixture.channels.channel(&fixture.settlement_id);
        assert_eq!(
            channel.entries, before_dispute.entries,
            "granting the key must not disturb a single stored entry — the \
             arbitrator reads the history, not a retelling of it"
        );
        let grant = channel.grants_for(&peer(&arbitrator))[0];
        assert_eq!(grant.role, GrantRole::Arbitrator);

        let recovered = ChannelKey::from_bytes(
            openfiat_crypto::open(&arbitrator, &grant.sealed_key)
                .expect("the grant is sealed to the arbitrator")
                .try_into()
                .unwrap(),
        );
        let opened: Vec<Vec<u8>> = channel
            .entries
            .iter()
            .map(|entry| open_entry(&recovered, &entry.binding(), &entry.payload).unwrap())
            .collect();
        assert!(opened.contains(&b"Equity Bank 0110123456789".to_vec()));
        assert!(opened.contains(&b"sent it".to_vec()));
    }

    /// Disclosure is all or nothing on purpose. A party who could hand
    /// over three of five messages would be handing over an argument
    /// rather than evidence.
    #[test]
    fn one_grant_discloses_the_whole_channel_including_the_payment_details() {
        let fixture = Fixture::new();
        let key = ChannelKey::generate();
        fixture
            .grant(&fixture.seller, &fixture.buyer, &key)
            .unwrap();
        fixture
            .post(&fixture.seller, 0, EntryKind::PaymentDetails, &key, b"acct")
            .unwrap();
        fixture
            .post(&fixture.seller, 1, EntryKind::Message, &key, b"any minute")
            .unwrap();

        let arbitrator = Keypair::generate();
        fixture.open_dispute_with(&arbitrator);
        fixture.grant(&fixture.seller, &arbitrator, &key).unwrap();

        let channel = fixture.channels.channel(&fixture.settlement_id);
        let grant = channel.grants_for(&peer(&arbitrator))[0];
        let recovered = ChannelKey::from_bytes(
            openfiat_crypto::open(&arbitrator, &grant.sealed_key)
                .unwrap()
                .try_into()
                .unwrap(),
        );
        assert_eq!(channel.payment_details().len(), 1);
        assert_eq!(channel.messages().len(), 1);
        for entry in &channel.entries {
            assert!(
                open_entry(&recovered, &entry.binding(), &entry.payload).is_ok(),
                "a single grant opens every kind of entry, by design"
            );
        }
    }

    /// Both parties may grant the same arbitrator. Keying grants by
    /// (recipient, granter) is what stops one party sealing garbage over
    /// the other party's honest grant and locking the arbitrator out.
    #[test]
    fn one_party_cannot_overwrite_the_others_grant_to_the_same_arbitrator() {
        let fixture = Fixture::new();
        let key = ChannelKey::generate();
        let arbitrator = Keypair::generate();
        fixture.open_dispute_with(&arbitrator);

        fixture.grant(&fixture.buyer, &arbitrator, &key).unwrap();
        // The seller grants a *different* key — the sabotage this layout
        // defends against.
        fixture
            .grant(&fixture.seller, &arbitrator, &ChannelKey::generate())
            .unwrap();

        let channel = fixture.channels.channel(&fixture.settlement_id);
        let grants = channel.grants_for(&peer(&arbitrator));
        assert_eq!(grants.len(), 2, "one grant per granter, neither displaced");
        assert!(
            grants.iter().any(|grant| grant.key_id == key.id()),
            "the honest grant survives the dishonest one"
        );
    }

    #[test]
    fn a_repeated_identical_entry_is_a_no_op_and_a_changed_one_is_refused() {
        let fixture = Fixture::new();
        let key = ChannelKey::generate();
        let author_id = peer(&fixture.seller);
        let binding = crate::key::EntryBinding {
            settlement_id: &fixture.settlement_id,
            author: &author_id,
            sequence: 0,
            kind: EntryKind::Message.name(),
        };
        let payload = seal_entry(&key, &binding, b"first").unwrap();
        let signed = SignedTradeChannelEntryPost::sign(
            TradeChannelEntryPost {
                settlement_id: fixture.settlement_id.clone(),
                author: author_id.clone(),
                sequence: 0,
                kind: EntryKind::Message,
                payload,
                timestamp: Timestamp::now(),
            },
            &fixture.seller,
        );

        // Gossip re-delivery: the same bytes twice must not be an error.
        fixture.channels.apply_entry(signed.clone()).unwrap();
        fixture.channels.apply_entry(signed).unwrap();
        assert_eq!(
            fixture
                .channels
                .channel(&fixture.settlement_id)
                .entries
                .len(),
            1
        );

        // Rewriting history at a sequence number already spent is not.
        assert_eq!(
            fixture.post(&fixture.seller, 0, EntryKind::Message, &key, b"second"),
            Err(TradeChannelError::SequenceReused)
        );
    }

    /// Each party owns its own run of sequence numbers, so neither can
    /// squat the other's slots or displace their entries.
    #[test]
    fn the_two_parties_sequence_numbers_do_not_collide() {
        let fixture = Fixture::new();
        let key = ChannelKey::generate();
        fixture
            .post(&fixture.seller, 0, EntryKind::Message, &key, b"from seller")
            .unwrap();
        fixture
            .post(&fixture.buyer, 0, EntryKind::Message, &key, b"from buyer")
            .unwrap();

        let channel = fixture.channels.channel(&fixture.settlement_id);
        assert_eq!(channel.entries.len(), 2);
        assert_eq!(channel.next_sequence(&peer(&fixture.seller)), 1);
        assert_eq!(channel.next_sequence(&peer(&fixture.buyer)), 1);
    }

    #[test]
    fn a_forged_signature_is_refused() {
        let fixture = Fixture::new();
        let impostor = Keypair::generate();
        let key = ChannelKey::generate();
        let author_id = peer(&fixture.seller);
        let binding = crate::key::EntryBinding {
            settlement_id: &fixture.settlement_id,
            author: &author_id,
            sequence: 0,
            kind: EntryKind::Message.name(),
        };
        let payload = seal_entry(&key, &binding, b"I approve").unwrap();
        // Claims to be the seller, signed by somebody else entirely.
        let signed = SignedTradeChannelEntryPost::sign(
            TradeChannelEntryPost {
                settlement_id: fixture.settlement_id.clone(),
                author: author_id.clone(),
                sequence: 0,
                kind: EntryKind::Message,
                payload,
                timestamp: Timestamp::now(),
            },
            &impostor,
        );
        assert_eq!(
            fixture.channels.apply_entry(signed),
            Err(TradeChannelError::InvalidSignature)
        );
    }

    #[test]
    fn an_entry_for_a_settlement_this_node_has_not_seen_is_refused() {
        let fixture = Fixture::new();
        let key = ChannelKey::generate();
        let author_id = peer(&fixture.seller);
        let unknown = SettlementId::new("settle-unknown");
        let binding = crate::key::EntryBinding {
            settlement_id: &unknown,
            author: &author_id,
            sequence: 0,
            kind: EntryKind::Message.name(),
        };
        let payload = seal_entry(&key, &binding, b"hello").unwrap();
        let signed = SignedTradeChannelEntryPost::sign(
            TradeChannelEntryPost {
                settlement_id: unknown.clone(),
                author: author_id.clone(),
                sequence: 0,
                kind: EntryKind::Message,
                payload,
                timestamp: Timestamp::now(),
            },
            &fixture.seller,
        );
        assert_eq!(
            fixture.channels.apply_entry(signed),
            Err(TradeChannelError::SettlementNotFound)
        );
    }

    /// An event every node stores forever must not be a place to park a
    /// megabyte. The bound is on the ciphertext because that is the only
    /// length a node can measure.
    #[test]
    fn an_oversized_payload_is_refused() {
        let fixture = Fixture::new();
        let author_id = peer(&fixture.seller);
        let signed = SignedTradeChannelEntryPost::sign(
            TradeChannelEntryPost {
                settlement_id: fixture.settlement_id.clone(),
                author: author_id,
                sequence: 0,
                kind: EntryKind::Message,
                payload: crate::key::ChannelCiphertext {
                    key_id: crate::key::ChannelKeyId::from_bytes([0u8; 8]),
                    nonce: [0u8; 12],
                    ciphertext: vec![0u8; MAX_ENTRY_CIPHERTEXT + 1],
                },
                timestamp: Timestamp::now(),
            },
            &fixture.seller,
        );
        assert_eq!(
            fixture.channels.apply_entry(signed),
            Err(TradeChannelError::EntryTooLarge)
        );
    }

    /// A prefix scan must not pick up a neighbouring channel whose id
    /// merely starts with the same bytes.
    #[test]
    fn a_channel_scan_does_not_pick_up_a_settlement_with_a_longer_similar_id() {
        let fixture = Fixture::new();
        let sibling = SettlementId::new("settle-10");
        fixture
            .settlements
            .apply_initiate(SignedSettlementInitiate::sign(
                SettlementInitiate {
                    id: sibling.clone(),
                    reservation_id: ReservationId::new("res-2"),
                    buyer: peer(&fixture.buyer),
                    buyer_public_key: fixture.buyer.public_key(),
                    seller: peer(&fixture.seller),
                    seller_public_key: fixture.seller.public_key(),
                    amount: Amount::new(1_000_000, 6),
                    timestamp: Timestamp::now(),
                },
                &fixture.buyer,
            ))
            .unwrap();

        let key = ChannelKey::generate();
        let author_id = peer(&fixture.seller);
        let binding = crate::key::EntryBinding {
            settlement_id: &sibling,
            author: &author_id,
            sequence: 0,
            kind: EntryKind::Message.name(),
        };
        let payload = seal_entry(&key, &binding, b"other trade").unwrap();
        fixture
            .channels
            .apply_entry(SignedTradeChannelEntryPost::sign(
                TradeChannelEntryPost {
                    settlement_id: sibling.clone(),
                    author: author_id.clone(),
                    sequence: 0,
                    kind: EntryKind::Message,
                    payload,
                    timestamp: Timestamp::now(),
                },
                &fixture.seller,
            ))
            .unwrap();

        assert!(
            fixture
                .channels
                .channel(&fixture.settlement_id)
                .entries
                .is_empty(),
            "settle-1 must not see settle-10's entries"
        );
        assert_eq!(fixture.channels.channel(&sibling).entries.len(), 1);
    }

    #[test]
    fn a_settlement_with_nothing_written_to_it_reads_as_an_empty_channel() {
        let fixture = Fixture::new();
        let channel = fixture.channels.channel(&fixture.settlement_id);
        assert!(channel.entries.is_empty());
        assert!(channel.readers().is_empty());
        assert_eq!(channel.next_sequence(&peer(&fixture.buyer)), 0);
    }
}
