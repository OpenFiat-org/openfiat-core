//! Reservation methods (OFS-2200).

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::methods::redaction::PublicReservation;
use crate::methods::wallet_auth::{WalletProof, verify_wallet};
use crate::state::NodeState;
use openfiat_reservations::events::SignedReservationRequest;
use openfiat_reservations::protocol;
use openfiat_reservations::{Reservation, ReservationId};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

/// Domain separator for `getMyReservations`.
pub const CHALLENGE_DOMAIN: &str = "openfiat-my-reservations";

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        // Redacted, like `getReservation(s)` on settlements and for the
        // same reason — and here the leak is a step earlier, since a
        // reservation names the buyer and its advertisement names the
        // merchant, so the edge exists even for trades that never settled.
        "getReservation",
        method_fn(
            |state: &NodeState<S>,
             params: IdParams|
             -> Result<Option<PublicReservation>, RpcError> {
                Ok(state
                    .reservations
                    .get(&ReservationId::new(params.id))
                    .map(PublicReservation::from))
            },
        ),
    );
    table.register(
        "getReservations",
        method_fn(
            |state: &NodeState<S>,
             _params: serde_json::Value|
             -> Result<Vec<PublicReservation>, RpcError> {
                Ok(state
                    .reservations
                    .all()
                    .into_iter()
                    .map(PublicReservation::from)
                    .collect())
            },
        ),
    );
    table.register(
        "getMyReservations",
        method_fn(
            |state: &NodeState<S>, params: WalletProof| -> Result<Vec<Reservation>, RpcError> {
                let wallet = verify_wallet(state, &params, CHALLENGE_DOMAIN)?;
                Ok(state
                    .reservations
                    .all()
                    .into_iter()
                    .filter(|r| r.requester == wallet)
                    .collect())
            },
        ),
    );
    table.register(
        "sendReservationRequest",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedReservationRequest =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedReservationRequest always serializes");
                let id = state
                    .reservations
                    .apply_request(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_REQUESTED,
                    protocol::OFS_SPEC,
                    Priority::SessionReservationSettlement,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
}
