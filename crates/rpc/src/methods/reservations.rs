//! Reservation methods (OFS-2200).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_reservations::events::SignedReservationRequest;
use openfiat_reservations::{Reservation, ReservationId};
use openfiat_serialization::wire;
use openfiat_storage::KvStore;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getReservation",
        method_fn(|state: &NodeState<S>, params: IdParams| -> Result<Option<Reservation>, RpcError> { Ok(state.reservations.get(&ReservationId::new(params.id))) }),
    );
    table.register("getReservations", method_fn(|state: &NodeState<S>, _params: serde_json::Value| -> Result<Vec<Reservation>, RpcError> { Ok(state.reservations.all()) }));
    table.register(
        "sendReservationRequest",
        method_fn(|state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
            let bytes = decode_bytes(&params.data)?;
            let signed: SignedReservationRequest = wire::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
            let id = state.reservations.apply_request(signed).map_err(|e| RpcError::Application(e.code()))?;
            Ok(id.as_str().to_string())
        }),
    );
}
