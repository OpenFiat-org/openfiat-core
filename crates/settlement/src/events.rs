//! Signed settlement events. `SettlementInitiate` is self-consistency
//! verified (signed by the buyer, whose successful reservation is what
//! begins settlement — §1); every other event is verified against
//! whichever party's public key is already on file for that settlement,
//! the same "check against the stored record's owner" pattern used
//! everywhere else in this workspace.

use crate::error::SettlementError;
use crate::record::SettlementId;
use openfiat_crypto::Keypair;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_reservations::ReservationId;
use openfiat_types::{Amount, PeerId, PublicKey, Signature, Timestamp};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SettlementInitiate {
    pub id: SettlementId,
    pub reservation_id: ReservationId,
    pub buyer: PeerId,
    pub buyer_public_key: PublicKey,
    pub seller: PeerId,
    pub seller_public_key: PublicKey,
    pub amount: Amount,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedSettlementInitiate {
    pub initiate: SettlementInitiate,
    pub signature: Signature,
}

impl SignedSettlementInitiate {
    pub fn sign(initiate: SettlementInitiate, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::wire::to_bytes(&initiate)
            .expect("SettlementInitiate always serializes");
        Self {
            signature: keypair.sign(&bytes),
            initiate,
        }
    }

    pub fn verify(&self) -> Result<(), SettlementError> {
        let expected = peer_id_from_public_key(&self.initiate.buyer_public_key)
            .map_err(|_| SettlementError::InvalidSignature)?;
        if expected != self.initiate.buyer {
            return Err(SettlementError::Unauthorized);
        }
        let bytes = openfiat_serialization::wire::to_bytes(&self.initiate)
            .map_err(|_| SettlementError::MalformedSettlement)?;
        openfiat_crypto::verify(&self.initiate.buyer_public_key, &bytes, &self.signature)
            .map_err(|_| SettlementError::InvalidSignature)
    }
}

macro_rules! settlement_action {
    ($unsigned:ident, $signed:ident { $( $field:ident: $ty:ty ),* $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct $unsigned {
            pub settlement_id: SettlementId,
            $( pub $field: $ty, )*
            pub timestamp: Timestamp,
        }

        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        pub struct $signed {
            pub action: $unsigned,
            pub signature: Signature,
        }

        impl $signed {
            pub fn sign(action: $unsigned, keypair: &Keypair) -> Self {
                let bytes = openfiat_serialization::wire::to_bytes(&action).expect(concat!(stringify!($unsigned), " always serializes"));
                Self { signature: keypair.sign(&bytes), action }
            }
        }
    };
}

settlement_action!(PaymentSubmitted, SignedPaymentSubmitted { buyer: PeerId, payment_reference: Option<String> });
settlement_action!(PaymentReversed, SignedPaymentReversed { buyer: PeerId });
settlement_action!(
    SettlementApproved,
    SignedSettlementApproved { seller: PeerId }
);
settlement_action!(
    SettlementRejected,
    SignedSettlementRejected {
        seller: PeerId,
        reason: String
    }
);
settlement_action!(
    SettlementCancelled,
    SignedSettlementCancelled { canceller: PeerId }
);
