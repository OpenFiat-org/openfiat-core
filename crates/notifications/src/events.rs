//! Signed notification events. `SubscriptionUpdate` is self-consistency
//! verified (the wallet owns its own subscription). `DeliveryReceipt` is
//! self-consistency verified here (the reporting peer really is who it
//! claims to be) and then checked against `openfiat-registry`'s on-file
//! provider for the referenced service at the store layer — the same
//! two-tier pattern used everywhere else in this workspace, except the
//! second tier looks up a different crate's registry instead of this
//! crate's own.

use crate::error::NotificationError;
use crate::record::{DeliveryStatus, NotificationCategory, NotificationId, NotificationTrigger};
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::{PeerId, PublicKey, ServiceId, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubscriptionUpdate {
    pub wallet: PeerId,
    pub wallet_public_key: PublicKey,
    pub enabled_categories: Vec<NotificationCategory>,
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
