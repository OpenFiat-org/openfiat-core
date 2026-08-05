//! The one signed payment-method event.
//!
//! Self-consistency verified, the shape `openfiat_reviews::
//! SignedReviewPublish` and `openfiat_identity`'s `SignedClaimPublish` both
//! use: the record carries the key it was signed with, that key must derive
//! to the `merchant` the record names (OFNP §6), and the signature must
//! verify against it. What this establishes is who wrote the definition —
//! which, for a record whose entire purpose is to be attributed to its
//! author, is also the whole of the authorization.
//!
//! There is no update event and no delete event. A definition is immutable
//! by construction (see [`crate::MerchantPaymentMethod::id`]), so an
//! update would have nothing to land on; a delete would be a promise this
//! network cannot keep, because every node already holds the bytes and an
//! advertisement may already reference them. A merchant who no longer
//! settles on a rail removes it from their advertisement's terms, which is
//! the record that actually decides what they will take.

use crate::error::TaxonomyError;
use crate::record::MerchantPaymentMethod;
use openfiat_crypto::{Keypair, verify};
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_types::Signature;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SignedPaymentMethodDefine {
    pub method: MerchantPaymentMethod,
    pub signature: Signature,
}

impl SignedPaymentMethodDefine {
    pub fn sign(method: MerchantPaymentMethod, keypair: &Keypair) -> Self {
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::PAYMENT_METHOD_DEFINE,
            &method,
        )
        .expect("a MerchantPaymentMethod always serializes");
        Self {
            signature: keypair.sign(&bytes),
            method,
        }
    }

    /// # Errors
    ///
    /// [`TaxonomyError::InvalidSignature`] if the signature does not
    /// verify, or if the key it was made with does not derive to the
    /// merchant the record names — a valid signature by a key that is not
    /// the claimed author's proves nothing about the author, so it is
    /// refused before the signature is even checked.
    pub fn verify(&self) -> Result<(), TaxonomyError> {
        let expected = peer_id_from_public_key(&self.method.merchant_public_key)
            .map_err(|_| TaxonomyError::InvalidSignature)?;
        if expected != self.method.merchant {
            return Err(TaxonomyError::InvalidSignature);
        }
        let bytes = openfiat_serialization::domain::preimage(
            openfiat_serialization::domain::tag::PAYMENT_METHOD_DEFINE,
            &self.method,
        )
        .map_err(|_| TaxonomyError::MalformedDefinition)?;
        verify(&self.method.merchant_public_key, &bytes, &self.signature)
            .map_err(|_| TaxonomyError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{keypair, merchant_method};

    #[test]
    fn a_definition_signed_by_its_author_verifies() {
        let signed = SignedPaymentMethodDefine::sign(merchant_method(1, "Acme Pay"), &keypair(1));
        assert_eq!(signed.verify(), Ok(()));
    }

    #[test]
    fn a_definition_signed_by_anyone_else_does_not() {
        let signed = SignedPaymentMethodDefine::sign(merchant_method(1, "Acme Pay"), &keypair(2));
        assert_eq!(signed.verify(), Err(TaxonomyError::InvalidSignature));
    }

    /// Naming a key that is genuinely yours while claiming to be a
    /// different wallet is the one combination a naive signature check
    /// would let through — and here it would put somebody else's name on
    /// a rail, under an id prefixed with their peer id.
    #[test]
    fn a_key_that_does_not_derive_to_the_named_merchant_is_refused() {
        let mut method = merchant_method(1, "Acme Pay");
        method.merchant_public_key = keypair(2).public_key();
        let signed = SignedPaymentMethodDefine::sign(method, &keypair(2));
        assert_eq!(signed.verify(), Err(TaxonomyError::InvalidSignature));
    }

    #[test]
    fn changing_the_name_after_signing_invalidates_it() {
        let mut signed =
            SignedPaymentMethodDefine::sign(merchant_method(1, "Acme Pay"), &keypair(1));
        signed.method.name = "M-Pesa Kenya (Safaricom)".to_string();
        assert_eq!(signed.verify(), Err(TaxonomyError::InvalidSignature));
    }
}
