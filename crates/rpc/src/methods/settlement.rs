//! Settlement methods (OFS-2300), and the confidential trade channel
//! that hangs off a settlement (`openfiat-tradechannel`).
//!
//! The channel's three methods live here rather than in a module of their
//! own because a channel has no identity apart from its settlement: it is
//! addressed by settlement id, authorized by the settlement's parties,
//! and its events ride the settlement spec.

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::methods::redaction::PublicSettlement;
use crate::methods::wallet_auth::{WalletProof, verify_wallet};
use crate::state::NodeState;
use openfiat_serialization::{json, wire};
use openfiat_settlement::events::{
    SignedPaymentSubmitted, SignedSettlementApproved, SignedSettlementCancelled,
    SignedSettlementInitiate, SignedSettlementRejected,
};
use openfiat_settlement::{Settlement, SettlementId, protocol};
use openfiat_storage::KvStore;
use openfiat_tradechannel::events::{SignedTradeChannelEntryPost, SignedTradeChannelKeyGrant};
use openfiat_tradechannel::{TradeChannel, protocol as channel_protocol};
use openfiat_types::{ErrorCode, Priority};

/// Domain separator for `getMySettlements`. A signature collected on
/// another gated surface can never be presented here, even though both
/// draw their nonces from the same ledger.
pub const CHALLENGE_DOMAIN: &str = "openfiat-my-settlements";

/// Domain separator for `getMyTradeChannel`, distinct from
/// `CHALLENGE_DOMAIN` for the same reason every other gated read has its
/// own: a signature a wallet made to list its settlements must not also
/// open its conversations.
pub const CHANNEL_CHALLENGE_DOMAIN: &str = "openfiat-my-trade-channel";

/// Params for `getMyTradeChannel`: which channel, and proof of who is
/// asking.
///
/// The proof is flattened rather than nested so this method's params look
/// exactly like every other wallet-proof method's, with one extra field.
#[derive(serde::Deserialize)]
pub struct TradeChannelParams {
    /// The settlement id the channel belongs to.
    pub id: String,
    #[serde(flatten)]
    pub proof: WalletProof,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        // Both reads are redacted, and the by-id one is not an
        // afterthought: the enumerating method hands out every id, so a
        // redaction that left `getSettlement` whole would be bypassed by
        // iterating the list it returns.
        "getSettlement",
        method_fn(
            |state: &NodeState<S>,
             params: IdParams|
             -> Result<Option<PublicSettlement>, RpcError> {
                Ok(state
                    .settlements
                    .get(&SettlementId::new(params.id))
                    .map(PublicSettlement::from))
            },
        ),
    );
    table.register(
        "getSettlements",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<PublicSettlement>, RpcError> {
                Ok(state
                    .settlements
                    .all()
                    .into_iter()
                    .map(PublicSettlement::from)
                    .collect())
            },
        ),
    );
    table.register(
        "getMySettlements",
        method_fn(
            |state: &NodeState<S>, params: WalletProof| -> Result<Vec<Settlement>, RpcError> {
                let wallet = verify_wallet(state, &params, CHALLENGE_DOMAIN)?;
                // Unredacted, and only for trades this wallet is in. A
                // party already knows who they traded with; nothing is
                // disclosed to them that they did not take part in.
                Ok(state
                    .settlements
                    .all()
                    .into_iter()
                    .filter(|s| s.buyer == wallet || s.seller == wallet)
                    .collect())
            },
        ),
    );
    table.register(
        "sendSettlementInitiate",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSettlementInitiate =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedSettlementInitiate always serializes");
                let id = state
                    .settlements
                    .apply_initiate(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_INITIATED,
                    protocol::OFS_SPEC,
                    Priority::SessionReservationSettlement,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
    table.register(
        "sendPaymentSubmitted",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedPaymentSubmitted =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedPaymentSubmitted always serializes");
                state
                    .settlements
                    .apply_payment_submitted(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_PAYMENT_SUBMITTED,
                    protocol::OFS_SPEC,
                    Priority::SessionReservationSettlement,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    table.register(
        "sendSettlementApproved",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSettlementApproved =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedSettlementApproved always serializes");
                state
                    .settlements
                    .apply_approved(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_APPROVED,
                    protocol::OFS_SPEC,
                    Priority::SessionReservationSettlement,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    table.register(
        // The merchant's "no" — the other half of `sendSettlementApproved`,
        // and until now the half with no way to say it.
        //
        // A merchant who could not find the buyer's payment had exactly
        // one lever on this surface: open a dispute. That drags in
        // arbitrators, a filing fee and a frozen escrow to express
        // something the protocol already models as a plain rejection, and
        // it charged the merchant for the privilege of refusing a payment
        // that never arrived.
        //
        // Legal only from `PaymentSubmitted`, and only under the seller's
        // key on file — a merchant cannot pre-emptively reject a
        // settlement the buyer has not yet declared payment on, because
        // there is nothing to reject yet, and the buyer cannot reject
        // their own.
        //
        // `Rejected` is not the end of the road for a buyer who really
        // did pay: `DisputeRegistry::apply_open` accepts a settlement in
        // any state, so the dispute path stays open afterwards. Rejection
        // moves the *cost* of escalating onto whichever party is actually
        // wrong, instead of charging the merchant to say no.
        "sendSettlementRejected",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSettlementRejected =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedSettlementRejected always serializes");
                state
                    .settlements
                    .apply_rejected(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_REJECTED,
                    protocol::OFS_SPEC,
                    Priority::SessionReservationSettlement,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    table.register(
        // §19: either party walks away, but only before payment is
        // declared. `apply_cancelled` picks the signing key by matching
        // the `canceller` field against the settlement's own buyer and
        // seller, so a stranger naming themselves canceller is
        // `Unauthorized` before any signature is checked, and a party
        // signing under someone else's name fails the check that follows.
        //
        // The `AwaitingPayment` restriction is what stops this being a
        // theft primitive: once the buyer has declared payment the
        // merchant's only exits are approval, rejection, or a dispute —
        // none of which can be taken unilaterally and silently.
        //
        // What it does not stop, because nothing in the protocol can, is
        // a merchant cancelling in the gap between a buyer wiring fiat
        // and that buyer pressing "I paid". A client should submit
        // `sendPaymentSubmitted` before the money leaves, not after it
        // lands — the declaration is cheap and reversible
        // (`PaymentReversed`), and the window it closes is not.
        "sendSettlementCancelled",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSettlementCancelled =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedSettlementCancelled always serializes");
                state
                    .settlements
                    .apply_cancelled(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_CANCELLED,
                    protocol::OFS_SPEC,
                    Priority::SessionReservationSettlement,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    register_trade_channel(table);
}

/// The confidential channel attached to a settlement: the payment details
/// one party hands the other, and their conversation.
///
/// # Why the read is gated and the writes are not
///
/// Both `send` methods take an already-signed payload and the registry
/// refuses anything not signed by a party, so there is nothing an
/// unauthenticated caller can push through them — the same shape every
/// other `sendX` on this surface has.
///
/// The read is a different question. What it returns is ciphertext, and
/// that ciphertext is gossiped to every node anyway, so gating it is not
/// confidentiality and this module does not pretend otherwise. What it
/// protects is the *metadata*: who talked to whom, how much, and when.
/// That is the trade graph `methods::wallet_auth` exists to keep from
/// being one `curl` away, and a channel is the richest version of it on
/// this surface.
fn register_trade_channel<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getMyTradeChannel",
        method_fn(
            |state: &NodeState<S>, params: TradeChannelParams| -> Result<TradeChannel, RpcError> {
                let wallet = verify_wallet(state, &params.proof, CHANNEL_CHALLENGE_DOMAIN)?;
                let settlement_id = SettlementId::new(params.id);
                let channel = state.trade_channels.channel(&settlement_id);

                // Two ways in, and the second is the one that matters.
                //
                // A party is obvious. An arbitrator is not a party and
                // never becomes one, so without the grant check a
                // disclosed channel would be unreachable through the very
                // interface the disclosure exists to serve. Reading the
                // permission off the replicated grants — rather than
                // re-deriving "is this peer on a dispute over this
                // settlement" here — also keeps one definition of who may
                // read a channel, in the registry that enforces it.
                let is_party = state
                    .settlements
                    .get(&settlement_id)
                    .is_some_and(|s| s.buyer == wallet || s.seller == wallet);
                if !is_party && !channel.is_reader(&wallet) {
                    // Refusal, not a narrowed answer: see
                    // `methods::wallet_auth` on why a filtered response is
                    // the shape that silently stops filtering one day.
                    return Err(RpcError::Application(ErrorCode::InvalidIdentityClaim));
                }
                Ok(channel)
            },
        ),
    );
    table.register(
        "sendTradeChannelKeyGrant",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedTradeChannelKeyGrant =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedTradeChannelKeyGrant always serializes");
                state
                    .trade_channels
                    .apply_key_grant(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    channel_protocol::EVENT_KEY_GRANTED,
                    channel_protocol::OFS_SPEC,
                    Priority::SessionReservationSettlement,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    table.register(
        "sendTradeChannelEntry",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedTradeChannelEntryPost =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedTradeChannelEntryPost always serializes");
                state
                    .trade_channels
                    .apply_entry(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    channel_protocol::EVENT_ENTRY_POSTED,
                    channel_protocol::OFS_SPEC,
                    // Same class as the settlement events around it:
                    // payment details the buyer is waiting on are exactly
                    // as time-critical as the trade they unblock.
                    Priority::SessionReservationSettlement,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
}
