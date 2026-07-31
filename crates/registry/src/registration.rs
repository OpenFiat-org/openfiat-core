//! Signed service registration (OFS-1500 §5, §7-8).

use crate::branding::ServiceBranding;
use crate::error::RegistryError;
use crate::pricing::ServicePricing;
use crate::record::{HealthState, ServiceRecord};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, ServiceId, ServiceType, Signature, Timestamp};

/// The unsigned content of a service registration (§7's field list, minus
/// pricing being explicitly optional per §15).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Registration {
    pub service_id: ServiceId,
    pub service_type: ServiceType,
    pub provider: PeerId,
    pub provider_public_key: PublicKey,
    pub endpoints: Vec<String>,
    pub supported_ofs: Vec<u16>,
    /// Where this service says it is, or which regions it says it
    /// serves (§10 — "providers MAY advertise regions served").
    ///
    /// Self-declared and unverified, deliberately. Deriving it from the
    /// endpoint's IP was investigated under #173 and rejected: see
    /// `docs/region-is-declared.md`. In short, a GeoIP answer would be a
    /// precise answer to a different question (where the socket
    /// terminates, not who the service is for), it would differ between
    /// nodes and so break the property that every node derives the same
    /// registry from the same events, and it would present a guess as a
    /// fact. Consumers must render it as declared.
    pub region: Option<String>,
    pub capabilities: Vec<String>,
    /// What this service is called, what it looks like and where to read
    /// more about it (§9). Self-asserted — see [`ServiceBranding`].
    ///
    /// Optional because most services have nothing to say here, and an
    /// absent name is better than an invented one.
    pub branding: Option<ServiceBranding>,
    /// What this service charges, if anything. OFS-1500 §15 keeps pricing
    /// optional — plenty of providers are free — but a declared price has
    /// to be machine-readable to be billable at all (OFS-4100 §9.5).
    pub pricing: Option<ServicePricing>,
    /// Base58 Solana address earnings are payable to.
    ///
    /// Deliberately its own field rather than being derived from
    /// `provider_public_key`. That key is the node's gossip identity: a
    /// hot key living on an internet-facing daemon. Forcing payouts to it
    /// would mean a provider's earnings accrue to the one key they can
    /// least afford to keep online, with no way to receive at a cold
    /// wallet. They are also different things — the payout target is a
    /// Solana wallet, and the token account funds actually land in is an
    /// ATA derived from it and the mint.
    pub payout_wallet: Option<String>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedRegistration {
    pub registration: Registration,
    pub signature: Signature,
}

impl SignedRegistration {
    pub fn sign(registration: Registration, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&registration)
            .expect("Registration always serializes");
        Self {
            signature: keypair.sign(&bytes),
            registration,
        }
    }

    /// Verify the signature, that the claimed provider Peer ID actually
    /// derives from the claimed public key (§21 — same peer-poisoning
    /// defense used by `openfiat-discovery`'s advertisements), and that a
    /// service declaring a price also says where it is to be paid.
    pub fn verify(&self) -> Result<(), RegistryError> {
        if self.registration.pricing.is_some() && self.registration.payout_wallet.is_none() {
            return Err(RegistryError::PricingWithoutPayoutWallet);
        }
        // Checked on the way in, so branding this node would have
        // refused from its own operator cannot arrive from a peer
        // instead. `apply_event` funnels every gossiped registration
        // through here.
        if let Some(branding) = &self.registration.branding {
            branding.validate()?;
        }
        if let Some(endpoint) = self
            .registration
            .endpoints
            .iter()
            .find(|endpoint| is_unresolvable(endpoint))
        {
            let _ = endpoint;
            return Err(RegistryError::UnresolvableEndpoint);
        }
        let expected = peer_id_from_public_key(&self.registration.provider_public_key)
            .map_err(|_| RegistryError::InvalidSignature)?;
        if expected != self.registration.provider {
            return Err(RegistryError::UnauthorizedUpdate);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.registration)
            .map_err(|_| RegistryError::MalformedRegistration)?;
        verify(
            &self.registration.provider_public_key,
            &bytes,
            &self.signature,
        )
        .map_err(|_| RegistryError::InvalidSignature)
    }

    pub fn into_record(self) -> ServiceRecord {
        let r = self.registration;
        ServiceRecord {
            service_id: r.service_id,
            service_type: r.service_type,
            provider: r.provider,
            provider_public_key: r.provider_public_key,
            endpoints: r.endpoints,
            supported_ofs: r.supported_ofs,
            region: r.region,
            capabilities: r.capabilities,
            // Normalised to `None` when nothing was actually declared,
            // so a consumer has one shape for "said nothing" instead of
            // two that render differently.
            branding: r.branding.filter(|b| !b.is_empty()),
            pricing: r.pricing,
            payout_wallet: r.payout_wallet,
            health: HealthState::Online,
            registered_at: r.timestamp,
            last_health_update: r.timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registration(keypair: &Keypair, provider: PeerId) -> Registration {
        Registration {
            service_id: ServiceId::new("svc-1"),
            service_type: ServiceType::MarketData(openfiat_types::MarketDataService::FxOracle),
            provider,
            provider_public_key: keypair.public_key(),
            endpoints: vec!["/ip4/127.0.0.1/udp/4001/quic-v1".to_string()],
            supported_ofs: vec![1500, 7000],
            region: Some("Kenya".to_string()),
            capabilities: vec!["KES/USD".to_string()],
            branding: None,
            pricing: None,
            payout_wallet: None,
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn accepts_a_genuinely_self_consistent_signed_registration() {
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let signed = SignedRegistration::sign(registration(&keypair, provider), &keypair);
        assert!(signed.verify().is_ok());
    }

    #[test]
    fn rejects_a_provider_id_that_does_not_match_the_public_key() {
        let keypair = Keypair::generate();
        let wrong_provider = PeerId::from_bytes(vec![0, 0, 0]);
        let signed = SignedRegistration::sign(registration(&keypair, wrong_provider), &keypair);
        assert_eq!(signed.verify(), Err(RegistryError::UnauthorizedUpdate));
    }

    #[test]
    fn a_structured_price_survives_signing_and_verification() {
        use crate::pricing::{BillingUnit, ServicePricing};
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut reg = registration(&keypair, provider);
        reg.pricing = Some(ServicePricing {
            token_mint: "29w8TroBTYoaqrXBDcpv5L54VZRA8Kf7kU5U1cakvFdj".to_string(),
            amount: openfiat_types::Amount::new(2_500, 6),
            unit: BillingUnit::Request,
        });
        reg.payout_wallet = Some("EA8TyQ58C3eavg3ThRFTMu1KLyV9e1v2oTQubSBQ9s5z".to_string());

        let signed = SignedRegistration::sign(reg, &keypair);
        assert!(signed.verify().is_ok());
        let record = signed.into_record();
        assert_eq!(record.pricing.unwrap().amount.base_units(), 2_500);
        assert!(record.payout_wallet.is_some());
    }

    #[test]
    fn a_price_without_a_payout_wallet_is_rejected() {
        use crate::pricing::{BillingUnit, ServicePricing};
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut reg = registration(&keypair, provider);
        reg.pricing = Some(ServicePricing {
            token_mint: "MINT".to_string(),
            amount: openfiat_types::Amount::new(1, 6),
            unit: BillingUnit::Month,
        });
        reg.payout_wallet = None;

        let signed = SignedRegistration::sign(reg, &keypair);
        assert_eq!(
            signed.verify(),
            Err(RegistryError::PricingWithoutPayoutWallet)
        );
    }

    #[test]
    fn a_free_service_still_registers() {
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let signed = SignedRegistration::sign(registration(&keypair, provider), &keypair);
        assert!(signed.verify().is_ok(), "pricing stays optional per §15");
    }

    #[test]
    fn declared_branding_survives_signing_and_reaches_the_record() {
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut reg = registration(&keypair, provider);
        reg.branding = Some(ServiceBranding {
            name: Some("AllenHark EU".to_string()),
            description: Some("Public API node, run by AllenHark.".to_string()),
            logo: Some("bafkreibdmq27skp3wnycoyoqcei47etyaulerpsegivlkfvyhjkw7ufjva".to_string()),
            website: Some("https://openfiat.allenhark.com".to_string()),
        });

        let signed = SignedRegistration::sign(reg, &keypair);
        assert_eq!(signed.verify(), Ok(()));
        let branding = signed
            .into_record()
            .branding
            .expect("branding must survive the round trip");
        assert_eq!(branding.name.as_deref(), Some("AllenHark EU"));
        assert_eq!(
            branding.website.as_deref(),
            Some("https://openfiat.allenhark.com")
        );
    }

    #[test]
    fn branding_this_node_would_refuse_cannot_be_gossiped_in_instead() {
        // The registration is genuinely signed by a genuine key: the
        // only thing wrong with it is the value. Signature verification
        // alone would accept it, which is why `verify` also validates
        // shape — every gossiped registration reaches the store through
        // this one path.
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut reg = registration(&keypair, provider);
        reg.branding = Some(ServiceBranding {
            logo: Some("https://tracker.example/pixel.png".to_string()),
            ..ServiceBranding::default()
        });

        let signed = SignedRegistration::sign(reg, &keypair);
        assert_eq!(signed.verify(), Err(RegistryError::MalformedBranding));
    }

    #[test]
    fn branding_that_declares_nothing_becomes_a_plain_absence_on_the_record() {
        // `Some(everything-None)` and `None` are the same statement, and
        // a consumer should not have to handle both spellings of it.
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut reg = registration(&keypair, provider);
        reg.branding = Some(ServiceBranding::default());

        let signed = SignedRegistration::sign(reg, &keypair);
        assert_eq!(signed.verify(), Ok(()));
        assert_eq!(signed.into_record().branding, None);
    }

    #[test]
    fn rejects_a_tampered_registration() {
        let keypair = Keypair::generate();
        let provider = peer_id_from_public_key(&keypair.public_key()).unwrap();
        let mut signed = SignedRegistration::sign(registration(&keypair, provider), &keypair);
        signed.registration.region = Some("Uganda".to_string());
        assert_eq!(signed.verify(), Err(RegistryError::InvalidSignature));
    }
}

/// Names reserved by RFC 2606 and RFC 6761, which are guaranteed never to
/// resolve for anyone.
///
/// `.localhost` is deliberately absent. RFC 6761 reserves it too, but
/// reserves it *to mean loopback* — it resolves, it works, and a node on a
/// developer's machine registering `http://localhost:7080` is describing
/// something genuinely reachable from where it runs. The others resolve
/// for nobody, ever.
const UNRESOLVABLE_SUFFIXES: [&str; 3] = [".test", ".invalid", ".example"];

/// Whether an endpoint names a host that can never be reached.
///
/// # Why the registry refuses these
///
/// A registration is a claim that a service can be found at an address,
/// and every consumer treats it as one: an interface lists it as a
/// provider a user can pick, a node may route to it, a browser offers a
/// button to connect. An address in a reserved domain cannot honour any
/// of that, so accepting it publishes a service that does not exist into
/// a registry whose entire value is that its entries are real.
///
/// This is not hypothetical. Five such registrations were seeded onto
/// devnet — `snapshots.eu.devnet.openfiat.test` and four siblings — and
/// they were served to users as live infrastructure, complete with a
/// button offering to connect to one. They were genuine signed records;
/// only their contents were invented, which is harder to spot than a
/// fixture and, unlike a fixture, replicated to every node on the
/// network.
///
/// Deliberately narrow: it rejects what is *provably* unreachable by
/// standard, not what happens to be down. A registry that dropped
/// services failing a liveness check would be making an availability
/// judgement, which is the consumer's to make — and `health` already
/// exists for it.
pub(crate) fn is_unresolvable(endpoint: &str) -> bool {
    let after_scheme = endpoint.split("://").nth(1).unwrap_or(endpoint);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    // Strip any port before matching, so `host.test:8443` is caught.
    let host = host.split(':').next().unwrap_or("").trim_end_matches('.');
    let host = host.to_ascii_lowercase();
    UNRESOLVABLE_SUFFIXES
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

#[cfg(test)]
mod endpoint_tests {
    use super::is_unresolvable;

    #[test]
    fn reserved_names_are_refused() {
        // The exact endpoints that were seeded onto devnet and served to
        // users as live infrastructure.
        for endpoint in [
            "https://snapshots.eu.devnet.openfiat.test",
            "https://rpc.us.devnet.openfiat.test",
            "https://fx.devnet.openfiat.test",
            "http://something.invalid",
            "https://foo.example",
            "https://HOST.TEST:8443/rpc",
        ] {
            assert!(is_unresolvable(endpoint), "{endpoint}");
        }
    }

    #[test]
    fn localhost_is_allowed_because_it_actually_resolves() {
        // RFC 6761 reserves `.localhost` to MEAN loopback. A node on a
        // developer's machine registering it is describing something
        // genuinely reachable from where it runs.
        for endpoint in [
            "http://localhost:7080",
            "http://api.localhost:7080",
            "http://127.0.0.1:7080",
        ] {
            assert!(!is_unresolvable(endpoint), "{endpoint}");
        }
    }

    #[test]
    fn ordinary_endpoints_pass() {
        for endpoint in [
            "https://openfiat.allenhark.com",
            "https://rpc.example.org/path",
            "https://10.0.0.4:7080",
            "https://testing.example.com",
        ] {
            assert!(!is_unresolvable(endpoint), "{endpoint}");
        }
    }

    #[test]
    fn a_hostname_merely_containing_test_is_not_reserved() {
        // Suffix matching, not substring: `.test` is a TLD, and
        // `latest.example.com` or `mytest.io` are ordinary names.
        for endpoint in ["https://latest.openfiat.network", "https://mytest.io"] {
            assert!(!is_unresolvable(endpoint), "{endpoint}");
        }
    }
}
