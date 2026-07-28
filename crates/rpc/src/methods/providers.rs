//! Service Registry methods (OFS-1500) — backs notification/oracle/
//! risk/snapshot provider discovery.

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_registry::{ServiceRecord, SignedRegistration, protocol};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::{Priority, ServiceId};

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getProvider",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<ServiceRecord>, RpcError> {
                Ok(state.services.get(&ServiceId::new(params.id)))
            },
        ),
    );
    table.register(
        "getProviders",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<ServiceRecord>, RpcError> { Ok(state.services.all()) },
        ),
    );
    table.register(
        "sendProviderRegister",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedRegistration =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedRegistration always serializes");
                let id = state
                    .services
                    .apply_registration(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_REGISTERED,
                    protocol::OFS_SPEC,
                    Priority::BackgroundSync,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
}
