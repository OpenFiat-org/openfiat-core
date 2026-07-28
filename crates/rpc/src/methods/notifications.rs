//! Notification methods (OFS-6000).

use crate::dispatch::{
    IdParams, MethodTable, SendEventParams, WalletParams, decode_bytes, decode_peer_id, method_fn,
};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_notifications::events::{SignedDeliveryReport, SignedSubscriptionUpdate};
use openfiat_notifications::{DeliveryReceipt, Subscription, protocol};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::Priority;

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getSubscription",
        method_fn(
            |state: &NodeState<S>,
             params: WalletParams|
             -> Result<Option<Subscription>, RpcError> {
                Ok(state
                    .notifications
                    .subscription(&decode_peer_id(&params.wallet)?))
            },
        ),
    );
    table.register(
        "getDeliveryReceiptsByWallet",
        method_fn(
            |state: &NodeState<S>,
             params: WalletParams|
             -> Result<Vec<DeliveryReceipt>, RpcError> {
                Ok(state
                    .notifications
                    .receipts_for(&decode_peer_id(&params.wallet)?))
            },
        ),
    );
    table.register(
        "getDeliveryReceipt",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<DeliveryReceipt>, RpcError> {
                Ok(state
                    .notifications
                    .receipt(&openfiat_notifications::NotificationId::new(params.id)))
            },
        ),
    );
    table.register(
        "sendSubscriptionUpdate",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedSubscriptionUpdate =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedSubscriptionUpdate always serializes");
                state
                    .notifications
                    .apply_subscription_update(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_SUBSCRIPTION_UPDATED,
                    protocol::OFS_SPEC,
                    Priority::Reputation,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    table.register(
        "sendDeliveryReport",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedDeliveryReport =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedDeliveryReport always serializes");
                let event_type = signed.report.status.event_type_name();
                state
                    .notifications
                    .apply_delivery_report(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    event_type,
                    protocol::OFS_SPEC,
                    Priority::Reputation,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
}
