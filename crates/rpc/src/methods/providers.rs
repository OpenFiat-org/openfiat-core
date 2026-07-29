//! Service Registry methods (OFS-1500) — backs notification/oracle/
//! risk/snapshot provider discovery.

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_registry::{
    ServiceRecord, SignedHealthUpdate, SignedRegistration, SignedWithdrawal, protocol,
};
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
    // §11: a provider proves liveness by publishing health updates. Until
    // this method existed a registration could never be refreshed, which is
    // why `expire_stale` could not safely be switched on — see
    // `actor::REGISTRY_EXPIRATION_THRESHOLD`.
    table.register(
        "sendProviderHealthUpdate",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedHealthUpdate =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedHealthUpdate always serializes");
                state
                    .services
                    .apply_health_update(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_UPDATED,
                    protocol::OFS_SPEC,
                    Priority::BackgroundSync,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
    // §17: voluntary withdrawal. The registry verifies the signature against
    // the key already on file, so a third party cannot deregister someone
    // else's service.
    table.register(
        "sendProviderWithdraw",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<(), RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedWithdrawal =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedWithdrawal always serializes");
                state
                    .services
                    .apply_withdrawal(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_UNREGISTERED,
                    protocol::OFS_SPEC,
                    Priority::BackgroundSync,
                    gossip_bytes,
                );
                Ok(())
            },
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{MethodTable, encode_bytes};
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_registry::health::{HealthState, HealthUpdate};
    use openfiat_registry::registration::Registration;
    use openfiat_registry::withdrawal::Withdrawal;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{InfrastructureService, ServiceType, Timestamp};
    use std::time::Duration;

    fn table_and_state() -> (MethodTable<MemoryStore>, NodeState<MemoryStore>) {
        let mut table = MethodTable::new();
        register(&mut table);
        (table, NodeState::new_for_test(MemoryStore::new()))
    }

    /// Signed payloads reach a `sendX` method base64-encoded, exactly as a
    /// real wallet-signing client would send them.
    fn params(signed: &impl serde::Serialize) -> serde_json::Value {
        let bytes = json::to_bytes(signed).unwrap();
        serde_json::json!({ "data": encode_bytes(&bytes) })
    }

    fn registration_at(keypair: &Keypair, id: &str, at: Timestamp) -> Registration {
        Registration {
            service_id: ServiceId::new(id),
            service_type: ServiceType::Infrastructure(InfrastructureService::SnapshotProvider),
            provider: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            provider_public_key: keypair.public_key(),
            endpoints: vec!["/ip4/127.0.0.1/udp/4001/quic-v1".to_string()],
            supported_ofs: vec![1500, 1300],
            region: None,
            capabilities: vec![],
            pricing: None,
            timestamp: at,
        }
    }

    fn register_service(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        keypair: &Keypair,
        id: &str,
        at: Timestamp,
    ) -> ServiceId {
        let signed = SignedRegistration::sign(registration_at(keypair, id, at), keypair);
        table
            .dispatch(state, "sendProviderRegister", params(&signed))
            .expect("registration must be accepted");
        ServiceId::new(id)
    }

    fn health_update(keypair: &Keypair, id: &ServiceId, at: Timestamp) -> SignedHealthUpdate {
        SignedHealthUpdate::sign(
            HealthUpdate {
                service_id: id.clone(),
                provider: peer_id_from_public_key(&keypair.public_key()).unwrap(),
                state: HealthState::Online,
                timestamp: at,
            },
            keypair,
        )
    }

    #[test]
    fn a_health_update_refreshes_the_liveness_timestamp_expiry_reads() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let registered_at = Timestamp::from_millis(1_000);
        let id = register_service(&table, &state, &owner, "svc-1", registered_at);
        assert_eq!(
            state.services.get(&id).unwrap().last_health_update,
            registered_at,
            "a fresh registration starts with last_health_update == registered_at"
        );

        let later = Timestamp::from_millis(500_000);
        table
            .dispatch(
                &state,
                "sendProviderHealthUpdate",
                params(&health_update(&owner, &id, later)),
            )
            .expect("the owner's own health update must be accepted");

        assert_eq!(
            state.services.get(&id).unwrap().last_health_update,
            later,
            "the health update must move the timestamp expire_stale compares against"
        );
    }

    #[test]
    fn a_health_update_signed_by_an_impostor_is_rejected() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let id = register_service(
            &table,
            &state,
            &owner,
            "svc-1",
            Timestamp::from_millis(1_000),
        );

        // The attacker names the real provider but signs with its own key.
        let attacker = Keypair::generate();
        let forged = SignedHealthUpdate::sign(
            HealthUpdate {
                service_id: id.clone(),
                provider: peer_id_from_public_key(&owner.public_key()).unwrap(),
                state: HealthState::Offline,
                timestamp: Timestamp::from_millis(500_000),
            },
            &attacker,
        );

        assert!(
            table
                .dispatch(&state, "sendProviderHealthUpdate", params(&forged))
                .is_err(),
            "a health update must be verified against the key already on file"
        );
        assert_eq!(
            state.services.get(&id).unwrap().last_health_update,
            Timestamp::from_millis(1_000),
            "a rejected update must not refresh liveness"
        );
    }

    #[test]
    fn a_withdrawal_from_the_owner_removes_the_service() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let id = register_service(
            &table,
            &state,
            &owner,
            "svc-1",
            Timestamp::from_millis(1_000),
        );

        let signed = SignedWithdrawal::sign(
            Withdrawal {
                service_id: id.clone(),
                provider: peer_id_from_public_key(&owner.public_key()).unwrap(),
                timestamp: Timestamp::from_millis(2_000),
            },
            &owner,
        );
        table
            .dispatch(&state, "sendProviderWithdraw", params(&signed))
            .expect("the owner's own withdrawal must be accepted");

        assert!(state.services.get(&id).is_none());
    }

    #[test]
    fn a_withdrawal_signed_by_an_impostor_is_rejected() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let id = register_service(
            &table,
            &state,
            &owner,
            "svc-1",
            Timestamp::from_millis(1_000),
        );

        let attacker = Keypair::generate();
        let forged = SignedWithdrawal::sign(
            Withdrawal {
                service_id: id.clone(),
                provider: peer_id_from_public_key(&owner.public_key()).unwrap(),
                timestamp: Timestamp::from_millis(2_000),
            },
            &attacker,
        );

        assert!(
            table
                .dispatch(&state, "sendProviderWithdraw", params(&forged))
                .is_err(),
            "a third party must not be able to deregister someone else's service"
        );
        assert!(
            state.services.get(&id).is_some(),
            "the service must survive a forged withdrawal"
        );
    }

    /// The interlock this whole change exists to unlock: before
    /// `sendProviderHealthUpdate` existed nothing could refresh
    /// `last_health_update`, so any sweep would eventually evict every
    /// provider. Heartbeating must keep a live provider alive.
    #[test]
    fn a_provider_that_keeps_heartbeating_survives_a_sweep_that_evicts_a_silent_one() {
        let (table, state) = table_and_state();
        let live = Keypair::generate();
        let silent = Keypair::generate();

        let long_ago = Timestamp::from_millis(1_000);
        let live_id = register_service(&table, &state, &live, "svc-live", long_ago);
        let silent_id = register_service(&table, &state, &silent, "svc-silent", long_ago);

        // Only the live provider heartbeats, and does so now.
        table
            .dispatch(
                &state,
                "sendProviderHealthUpdate",
                params(&health_update(&live, &live_id, Timestamp::now())),
            )
            .unwrap();

        let removed = state.services.expire_stale(Duration::from_secs(60));
        assert_eq!(removed, 1, "exactly the silent provider must be expired");
        assert!(
            state.services.get(&live_id).is_some(),
            "a heartbeating provider must never be swept"
        );
        assert!(
            state.services.get(&silent_id).is_none(),
            "a provider past the threshold must be swept"
        );
    }
}
