//! Settlement methods (OFS-2300).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_serialization::{json, wire};
use openfiat_settlement::events::{
    SignedPaymentSubmitted, SignedSettlementApproved, SignedSettlementInitiate,
};
use openfiat_settlement::{Settlement, SettlementId, protocol};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getSettlement",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<Settlement>, RpcError> {
                Ok(state.settlements.get(&SettlementId::new(params.id)))
            },
        ),
    );
    table.register(
        "getSettlements",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<Settlement>, RpcError> { Ok(state.settlements.all()) },
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
