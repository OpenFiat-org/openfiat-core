//! Advertisement methods (OFS-2100).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_advertisements::events::SignedAdvertisementCreate;
use openfiat_advertisements::{Advertisement, AdvertisementId};
use openfiat_serialization::json;
use openfiat_storage::KvStore;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getAdvertisement",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<Advertisement>, RpcError> {
                Ok(state.advertisements.get(&AdvertisementId::new(params.id)))
            },
        ),
    );
    table.register(
        "getAdvertisements",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<Advertisement>, RpcError> { Ok(state.advertisements.all()) },
        ),
    );
    table.register(
        "sendAdvertisementCreate",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedAdvertisementCreate =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let id = state
                    .advertisements
                    .apply_create(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                Ok(id.as_str().to_string())
            },
        ),
    );
}
