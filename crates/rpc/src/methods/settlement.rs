//! Settlement methods (OFS-2300).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::methods::redaction::PublicSettlement;
use crate::methods::wallet_auth::{WalletProof, verify_wallet};
use crate::state::NodeState;
use openfiat_serialization::{json, wire};
use openfiat_settlement::events::{
    SignedPaymentSubmitted, SignedSettlementApproved, SignedSettlementInitiate,
};
use openfiat_settlement::{Settlement, SettlementId, protocol};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

/// Domain separator for `getMySettlements`. A signature collected on
/// another gated surface can never be presented here, even though both
/// draw their nonces from the same ledger.
pub const CHALLENGE_DOMAIN: &str = "openfiat-my-settlements";

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
}
