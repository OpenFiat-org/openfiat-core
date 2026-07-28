//! Signed service withdrawal (OFS-1500 §17). Verified the same way as a
//! health update: against the key already on file, not a self-asserted one.

use openfiat_crypto::Keypair;
use openfiat_types::{PeerId, ServiceId, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Withdrawal {
    pub service_id: ServiceId,
    pub provider: PeerId,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedWithdrawal {
    pub withdrawal: Withdrawal,
    pub signature: Signature,
}

impl SignedWithdrawal {
    pub fn sign(withdrawal: Withdrawal, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&withdrawal)
            .expect("Withdrawal always serializes");
        Self {
            signature: keypair.sign(&bytes),
            withdrawal,
        }
    }
}
