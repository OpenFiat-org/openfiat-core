//! Service Registry methods (OFS-1500) — backs notification/oracle/
//! risk/snapshot provider discovery.

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_crypto::verify;
use openfiat_registry::earnings::{EarningsChallenge, ProviderEarnings};
use openfiat_registry::{
    ServiceRecord, SignedHealthUpdate, SignedRegistration, SignedWithdrawal, protocol,
};
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_types::{Priority, ServiceId, Signature, Timestamp};

/// A provider answering an earnings challenge: which service, which nonce
/// they were issued, and their signature over the challenge's own bytes.
#[derive(serde::Deserialize)]
pub struct EarningsParams {
    pub id: String,
    pub nonce: String,
    /// Base64, matching every other signed payload on this surface.
    pub signature: String,
}

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
    // OFS-4100 §9.5: a provider reads their own earnings by proving control
    // of the Service ID, not by logging in. Step one hands out a random,
    // single-use, expiring nonce bound to that one service.
    //
    // Issuing is deliberately unauthenticated: a nonce is worthless without
    // the registered key to sign it, and demanding a signature to obtain the
    // thing you sign would be circular. It does confirm the service exists,
    // so a caller can tell "no such service" from "not yours".
    table.register(
        "getProviderEarningsChallenge",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<EarningsChallenge, RpcError> {
                let service_id = ServiceId::new(params.id);
                if state.services.get(&service_id).is_none() {
                    return Err(RpcError::Application(
                        openfiat_registry::RegistryError::ServiceNotFound.code(),
                    ));
                }
                Ok(state
                    .provider_earnings
                    .borrow_mut()
                    .issue_challenge(&service_id, Timestamp::now()))
            },
        ),
    );
    // Step two: the signature over the challenge is checked against the
    // public key the registry already holds for that service — the same
    // rule health updates and withdrawals follow, so a third party cannot
    // read someone else's statement any more than they could deregister it.
    table.register(
        "getProviderEarnings",
        method_fn(
            |state: &NodeState<S>, params: EarningsParams| -> Result<ProviderEarnings, RpcError> {
                let service_id = ServiceId::new(params.id);
                let record = state
                    .services
                    .get(&service_id)
                    .ok_or(RpcError::Application(
                        openfiat_registry::RegistryError::ServiceNotFound.code(),
                    ))?;

                // Consumed before the signature is checked, so presenting a
                // captured signature burns the nonce rather than replaying it.
                let challenge = state
                    .provider_earnings
                    .borrow_mut()
                    .consume_challenge(&service_id, &params.nonce, Timestamp::now())
                    .map_err(|e| RpcError::Application(e.code()))?;

                let raw: [u8; 64] = decode_bytes(&params.signature)?
                    .try_into()
                    .map_err(|_| RpcError::InvalidParams("signature must be 64 bytes".into()))?;
                let signature = Signature::from_bytes(raw);
                verify(
                    &record.provider_public_key,
                    &challenge.signing_bytes(),
                    &signature,
                )
                .map_err(|_| {
                    RpcError::Application(openfiat_registry::RegistryError::InvalidSignature.code())
                })?;

                Ok(state
                    .provider_earnings
                    .borrow()
                    .statement(&service_id, record.payout_wallet))
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
            payout_wallet: None,
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

    /// Drives the real two-step flow a provider performs: ask for a
    /// challenge, sign its bytes, present the signature.
    fn read_earnings(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        id: &ServiceId,
        signer: &Keypair,
    ) -> Result<serde_json::Value, crate::error::RpcError> {
        let challenge: EarningsChallenge = serde_json::from_value(
            table
                .dispatch(state, "getProviderEarningsChallenge", id_params(id))
                .expect("a challenge must be issued for a registered service"),
        )
        .unwrap();
        let signature = signer.sign(&challenge.signing_bytes());
        table.dispatch(
            state,
            "getProviderEarnings",
            serde_json::json!({
                "id": id.as_str(),
                "nonce": challenge.nonce,
                "signature": encode_bytes(&signature.as_bytes().expect("a freshly signed signature is always 64 bytes")),
            }),
        )
    }

    fn id_params(id: &ServiceId) -> serde_json::Value {
        serde_json::json!({ "id": id.as_str() })
    }

    #[test]
    fn a_provider_reads_its_own_statement_by_signing_the_challenge() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let id = register_service(
            &table,
            &state,
            &owner,
            "svc-1",
            Timestamp::from_millis(1_000),
        );

        let earnings: ProviderEarnings = serde_json::from_value(
            read_earnings(&table, &state, &id, &owner)
                .expect("the registered key must be able to read its own earnings"),
        )
        .unwrap();

        assert_eq!(earnings.service_id, id);
        assert!(
            earnings.entries.is_empty(),
            "no metering credits anything yet, so the statement is honestly empty"
        );
    }

    #[test]
    fn a_statement_cannot_be_read_by_a_key_that_does_not_own_the_service() {
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
        assert!(
            read_earnings(&table, &state, &id, &attacker).is_err(),
            "the signature must be checked against the key already on file"
        );
    }

    #[test]
    fn a_captured_signature_cannot_be_replayed() {
        let (table, state) = table_and_state();
        let owner = Keypair::generate();
        let id = register_service(
            &table,
            &state,
            &owner,
            "svc-1",
            Timestamp::from_millis(1_000),
        );

        // Capture a genuine, successful exchange off the wire.
        let challenge: EarningsChallenge = serde_json::from_value(
            table
                .dispatch(&state, "getProviderEarningsChallenge", id_params(&id))
                .unwrap(),
        )
        .unwrap();
        let signature = owner.sign(&challenge.signing_bytes());
        let replayed = serde_json::json!({
            "id": id.as_str(),
            "nonce": challenge.nonce,
            "signature": encode_bytes(&signature.as_bytes().expect("a freshly signed signature is always 64 bytes")),
        });

        assert!(
            table
                .dispatch(&state, "getProviderEarnings", replayed.clone())
                .is_ok(),
            "the first presentation is legitimate"
        );
        assert!(
            table
                .dispatch(&state, "getProviderEarnings", replayed)
                .is_err(),
            "the identical request must fail once the nonce is spent"
        );
    }

    #[test]
    fn a_challenge_is_not_issued_for_a_service_that_does_not_exist() {
        let (table, state) = table_and_state();
        assert!(
            table
                .dispatch(
                    &state,
                    "getProviderEarningsChallenge",
                    id_params(&ServiceId::new("nope")),
                )
                .is_err()
        );
    }

    /// A price only means something if it survives the whole path a real
    /// provider's registration takes: signed by them, gossiped as bytes,
    /// decoded into the record a client reads back.
    #[test]
    fn a_declared_price_round_trips_through_registration_to_the_stored_record() {
        use openfiat_registry::pricing::{BillingUnit, ServicePricing};
        let (table, state) = table_and_state();
        let owner = Keypair::generate();

        let mut reg = registration_at(&owner, "svc-priced", Timestamp::from_millis(1_000));
        reg.pricing = Some(ServicePricing {
            token_mint: "29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj".to_string(),
            amount: openfiat_types::Amount::new(50_000, 6),
            unit: BillingUnit::Request,
        });
        reg.payout_wallet = Some("EA8TyQ58C3eavg3ThRFTMu1KLyV9e1v2oTQubSBQ9s5z".to_string());
        table
            .dispatch(
                &state,
                "sendProviderRegister",
                params(&SignedRegistration::sign(reg, &owner)),
            )
            .expect("a priced registration must be accepted");

        let stored = state.services.get(&ServiceId::new("svc-priced")).unwrap();
        let price = stored
            .pricing
            .expect("the price must survive the round trip");
        assert_eq!(price.amount.base_units(), 50_000);
        assert_eq!(price.unit, BillingUnit::Request);
        assert_eq!(
            stored.payout_wallet.as_deref(),
            Some("EA8TyQ58C3eavg3ThRFTMu1KLyV9e1v2oTQubSBQ9s5z")
        );

        // And the statement reports where those funds would be payable.
        let earnings: ProviderEarnings = serde_json::from_value(
            read_earnings(&table, &state, &ServiceId::new("svc-priced"), &owner).unwrap(),
        )
        .unwrap();
        assert_eq!(
            earnings.payout_wallet.as_deref(),
            Some("EA8TyQ58C3eavg3ThRFTMu1KLyV9e1v2oTQubSBQ9s5z")
        );
    }

    #[test]
    fn a_price_without_a_payout_wallet_is_refused_at_the_rpc_boundary() {
        use openfiat_registry::pricing::{BillingUnit, ServicePricing};
        let (table, state) = table_and_state();
        let owner = Keypair::generate();

        let mut reg = registration_at(&owner, "svc-half", Timestamp::from_millis(1_000));
        reg.pricing = Some(ServicePricing {
            token_mint: "MINT".to_string(),
            amount: openfiat_types::Amount::new(1, 6),
            unit: BillingUnit::Month,
        });
        reg.payout_wallet = None;

        assert!(
            table
                .dispatch(
                    &state,
                    "sendProviderRegister",
                    params(&SignedRegistration::sign(reg, &owner)),
                )
                .is_err(),
            "billing with nowhere to be paid must not reach the registry"
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
