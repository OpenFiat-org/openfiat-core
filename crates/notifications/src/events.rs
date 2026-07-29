//! Signed notification events. `SubscriptionUpdate` is self-consistency
//! verified (the wallet owns its own subscription). `DeliveryReceipt` is
//! self-consistency verified here (the reporting peer really is who it
//! claims to be) and then checked against `openfiat-registry`'s on-file
//! provider for the referenced service at the store layer — the same
//! two-tier pattern used everywhere else in this workspace, except the
//! second tier looks up a different crate's registry instead of this
//! crate's own.

use crate::error::NotificationError;
use crate::record::{
    DeliveryStatus, NotificationCategory, NotificationId, NotificationTrigger,
    SubscriptionDestination,
};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, ServiceId, Signature, Timestamp};

/// §11's wallet-owned preferences, gossiped to the whole network.
///
/// `destinations` carries only sealed blobs (see
/// [`crate::record::SubscriptionDestination`]) precisely *because* this event is
/// replicated everywhere: a plaintext address here would be a permanent,
/// network-wide broadcast of the user's contact details.
///
/// The signature covers `destinations` automatically — `sign`/`verify`
/// serialize the whole struct, so adding a field extends the signed
/// message rather than leaving a gap.
/// `swapping_the_destinations_invalidates_the_signature` below pins that
/// behaviour, so a future field can't quietly fall outside the signature.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubscriptionUpdate {
    pub wallet: PeerId,
    pub wallet_public_key: PublicKey,
    pub enabled_categories: Vec<NotificationCategory>,
    /// `#[serde(default)]` for the same replica-compatibility reason as
    /// [`crate::record::Subscription::destinations`]: updates already on the wire have
    /// no such field and must still verify and apply.
    #[serde(default)]
    pub destinations: Vec<SubscriptionDestination>,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedSubscriptionUpdate {
    pub update: SubscriptionUpdate,
    pub signature: Signature,
}

impl SignedSubscriptionUpdate {
    pub fn sign(update: SubscriptionUpdate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&update)
            .expect("SubscriptionUpdate always serializes");
        Self {
            signature: keypair.sign(&bytes),
            update,
        }
    }

    pub fn verify(&self) -> Result<(), NotificationError> {
        let expected = peer_id_from_public_key(&self.update.wallet_public_key)
            .map_err(|_| NotificationError::InvalidSignature)?;
        if expected != self.update.wallet {
            return Err(NotificationError::Unauthorized);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.update)
            .map_err(|_| NotificationError::MalformedEvent)?;
        verify(&self.update.wallet_public_key, &bytes, &self.signature)
            .map_err(|_| NotificationError::InvalidSignature)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryReport {
    pub notification_id: NotificationId,
    pub service_id: ServiceId,
    pub provider: PeerId,
    pub provider_public_key: PublicKey,
    pub recipient_wallet: PeerId,
    pub trigger: NotificationTrigger,
    pub status: DeliveryStatus,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedDeliveryReport {
    pub report: DeliveryReport,
    pub signature: Signature,
}

impl SignedDeliveryReport {
    pub fn sign(report: DeliveryReport, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::json::to_bytes(&report)
            .expect("DeliveryReport always serializes");
        Self {
            signature: keypair.sign(&bytes),
            report,
        }
    }

    pub fn verify(&self) -> Result<(), NotificationError> {
        let expected = peer_id_from_public_key(&self.report.provider_public_key)
            .map_err(|_| NotificationError::InvalidSignature)?;
        if expected != self.report.provider {
            return Err(NotificationError::Unauthorized);
        }
        let bytes = openfiat_serialization::json::to_bytes(&self.report)
            .map_err(|_| NotificationError::MalformedEvent)?;
        verify(&self.report.provider_public_key, &bytes, &self.signature)
            .map_err(|_| NotificationError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::NotificationCategory;
    use openfiat_crypto::seal;
    use openfiat_types::NotificationChannel;

    fn destination(gateway: &Keypair) -> SubscriptionDestination {
        SubscriptionDestination {
            service_id: ServiceId::new("gateway-1"),
            channel: NotificationChannel::Email,
            sealed: seal(&gateway.public_key(), b"user@example.com").unwrap(),
        }
    }

    fn update(wallet: &Keypair, destinations: Vec<SubscriptionDestination>) -> SubscriptionUpdate {
        SubscriptionUpdate {
            wallet: peer_id_from_public_key(&wallet.public_key()).unwrap(),
            wallet_public_key: wallet.public_key(),
            enabled_categories: vec![NotificationCategory::Trading],
            destinations,
            timestamp: Timestamp::now(),
        }
    }

    #[test]
    fn a_genuine_update_with_destinations_verifies() {
        let wallet = Keypair::generate();
        let gateway = Keypair::generate();
        let signed =
            SignedSubscriptionUpdate::sign(update(&wallet, vec![destination(&gateway)]), &wallet);
        assert_eq!(signed.verify(), Ok(()));
    }

    /// The directive assumed the signature would cover `destinations`
    /// automatically because the whole struct is serialized. It does —
    /// this pins it, so nobody can swap a user's bound gateway for their
    /// own and keep the wallet's signature.
    #[test]
    fn swapping_the_destinations_invalidates_the_signature() {
        let wallet = Keypair::generate();
        let honest_gateway = Keypair::generate();
        let attacker_gateway = Keypair::generate();

        let mut signed = SignedSubscriptionUpdate::sign(
            update(&wallet, vec![destination(&honest_gateway)]),
            &wallet,
        );
        signed.update.destinations = vec![destination(&attacker_gateway)];

        assert_eq!(signed.verify(), Err(NotificationError::InvalidSignature));
    }

    /// Same check for the empty case: a stripped destination list is
    /// still a modification, not a silently-tolerated downgrade.
    #[test]
    fn stripping_the_destinations_invalidates_the_signature() {
        let wallet = Keypair::generate();
        let gateway = Keypair::generate();
        let mut signed =
            SignedSubscriptionUpdate::sign(update(&wallet, vec![destination(&gateway)]), &wallet);
        signed.update.destinations.clear();
        assert_eq!(signed.verify(), Err(NotificationError::InvalidSignature));
    }
}
