//! Session methods (OFS-1400).

use crate::dispatch::{
    IdParams, MethodTable, SendEventParams, WalletParams, decode_bytes, decode_peer_id, method_fn,
};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_serialization::wire;
use openfiat_sessions::events::{
    SignedSessionCreate, SignedSessionMigrate, SignedSessionRenew, SignedSessionRevoke,
};
use openfiat_sessions::{Session, SessionId};
use openfiat_storage::KvStore;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getSession",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<Session>, RpcError> {
                Ok(state.sessions.get(&SessionId::new(params.id)))
            },
        ),
    );
    table.register(
        "getSessionsByWallet",
        method_fn(
            |state: &NodeState<S>, params: WalletParams| -> Result<Vec<Session>, RpcError> {
                Ok(state
                    .sessions
                    .find_by_wallet(&decode_peer_id(&params.wallet)?))
            },
        ),
    );
    table.register(
        "sendSessionEstablish",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSessionCreate =
                    wire::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let id = state
                    .sessions
                    .apply_create(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                Ok(id.as_str().to_string())
            },
        ),
    );
    table.register(
        "sendSessionRenew",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSessionRenew =
                    wire::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                state
                    .sessions
                    .apply_renew(signed)
                    .map_err(|e| RpcError::Application(e.code()))
            },
        ),
    );
    table.register(
        "sendSessionRevoke",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSessionRevoke =
                    wire::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                state
                    .sessions
                    .apply_revoke(signed)
                    .map_err(|e| RpcError::Application(e.code()))
            },
        ),
    );
    table.register(
        "sendSessionMigrate",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSessionMigrate =
                    wire::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                state
                    .sessions
                    .apply_migrate(signed)
                    .map_err(|e| RpcError::Application(e.code()))
            },
        ),
    );
}
