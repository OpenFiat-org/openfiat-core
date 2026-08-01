//! Deterministic test values, in one place so a change of shape is one
//! edit rather than one per test module.

use crate::record::{MerchantPaymentMethod, PaymentMethodCategory};
use openfiat_crypto::Keypair;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::PeerId;

/// A keypair that is the same on every run, so an id derived from it is
/// the same in every assertion.
pub fn keypair(seed: u8) -> Keypair {
    Keypair::from_seed([seed; 32])
}

pub fn peer(seed: u8) -> PeerId {
    peer_id_from_public_key(&keypair(seed).public_key()).expect("a real key derives a peer id")
}

pub fn merchant_method(seed: u8, name: &str) -> MerchantPaymentMethod {
    MerchantPaymentMethod {
        merchant: peer(seed),
        merchant_public_key: keypair(seed).public_key(),
        name: name.to_string(),
        category: PaymentMethodCategory::BankTransfer,
    }
}
