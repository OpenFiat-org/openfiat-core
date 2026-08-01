//! Payment methods (OFS-2100): what to offer a merchant in their country,
//! what they have defined for themselves, and how to turn the id on an
//! advertisement back into a name.
//!
//! # Three methods, and why not fewer
//!
//! `getReferenceData` already returns the whole compiled-in catalog, and
//! a client could filter it locally. [`getPaymentMethods`] exists because
//! two of the three things a picker needs are not in that answer: the
//! split between "suggested here" and "everything else" is a decision this
//! node makes, and a merchant's own definitions are replicated state that
//! no compiled-in table can carry.
//!
//! [`getPaymentMethod`] answers the other direction. An advertisement
//! carries method *ids*, so a buyer reading somebody else's advertisement
//! has an id and needs a name — and the merchant-defined half of that
//! namespace cannot be resolved from any table a client ships.
//!
//! [`sendPaymentMethodDefine`]: the publish path, shaped like every other
//! `sendX` here — the caller signs, this node applies and re-gossips.
//!
//! # What this surface will not do
//!
//! Search another merchant's definitions to offer them to you. A
//! definition is selectable only by the merchant who wrote it (see
//! `openfiat_taxonomy::PaymentMethodRef::is_selectable_by`), so a method
//! returning "rails other merchants have invented" would be a list of
//! things the caller cannot choose — and a browsable index of arbitrary
//! merchant text, which is the shape of every name-squatting problem
//! scoping the namespace exists to avoid.
//!
//! [`getPaymentMethods`]: register
//! [`getPaymentMethod`]: register
//! [`sendPaymentMethodDefine`]: register

use crate::dispatch::{IdParams, MethodTable, SendEventParams, decode_bytes, method_fn};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_serialization::{json, wire};
use openfiat_storage::KvStore;
use openfiat_taxonomy::{
    PaymentMethod, PaymentMethodRef, SignedPaymentMethodDefine, for_country, protocol,
};
use openfiat_types::Priority;

/// What a picker asks for.
#[derive(Debug, serde::Deserialize)]
pub struct PickerParams {
    /// The merchant's country, as a code from `getReferenceData`. Absent
    /// means "nothing to suggest" rather than an error — see
    /// [`PaymentMethodChoices::suggested`].
    #[serde(default)]
    pub country: Option<String>,
    /// The merchant whose own definitions to include, base64-encoded like
    /// every other `wallet` parameter on this surface.
    ///
    /// Open, not gated. A definition is a public record that replicates to
    /// every node, so requiring a signature to read one would be
    /// theatre — and would stop a counterparty resolving what an
    /// advertisement means.
    #[serde(default)]
    pub wallet: Option<String>,
}

/// Everything a merchant's payment-method picker needs, in one answer.
#[derive(Debug, serde::Serialize)]
pub struct PaymentMethodChoices {
    /// Echoed back, so a client rendering a cached answer can tell which
    /// country it was for.
    pub country: Option<String>,
    /// The rails this build suggests in `country`, catalog order. Empty
    /// when no country was given, or when this build has nothing listed
    /// for it — which is an answer, not a failure.
    pub suggested: Vec<PaymentMethod>,
    /// Every other compiled-in rail, still selectable. A suggestion is an
    /// ordering; nothing here is withheld, because a merchant who settles
    /// over a corridor this build did not anticipate must still be able to
    /// say so.
    pub others: Vec<PaymentMethod>,
    /// The definitions `wallet` has published, id order. Empty when no
    /// wallet was given.
    ///
    /// Selectable only by that wallet — see the module doc. A client
    /// showing these to anyone else must show them as that merchant's own.
    pub merchant: Vec<PaymentMethod>,
}

pub fn register<S: KvStore + 'static>(table: &mut MethodTable<S>) {
    table.register(
        "getPaymentMethods",
        method_fn(
            |state: &NodeState<S>,
             params: PickerParams|
             -> Result<PaymentMethodChoices, RpcError> {
                let (suggested, others) = for_country(params.country.as_deref());
                let merchant = match &params.wallet {
                    None => Vec::new(),
                    Some(encoded) => state
                        .payment_methods
                        .for_merchant(&crate::dispatch::decode_peer_id(encoded)?)
                        .into_iter()
                        .map(|(_, method)| method.published())
                        .collect(),
                };
                Ok(PaymentMethodChoices {
                    country: params.country,
                    suggested: suggested.into_iter().cloned().collect(),
                    others: others.into_iter().cloned().collect(),
                    merchant,
                })
            },
        ),
    );
    table.register(
        "getPaymentMethod",
        method_fn(
            |state: &NodeState<S>, params: IdParams| -> Result<Option<PaymentMethod>, RpcError> {
                // An id that is not one at all is a malformed request, and
                // is worth saying so: a client that sent a display name
                // here would otherwise read `null` as "unknown rail" and
                // never find the bug.
                let id = PaymentMethodRef::parse(&params.id)
                    .map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                if id.owner().is_none() {
                    return Ok(openfiat_taxonomy::catalog()
                        .iter()
                        .find(|method| method.id == id)
                        .cloned());
                }
                // `None` for a definition this node has not received is
                // an ordinary answer, not an error: gossip may not have
                // delivered it yet, and an advertisement naming it is
                // valid either way.
                Ok(state
                    .payment_methods
                    .get(&id)
                    .map(|method| method.published()))
            },
        ),
    );
    table.register(
        "sendPaymentMethodDefine",
        method_fn(
            |state: &NodeState<S>, params: SendEventParams| -> Result<String, RpcError> {
                let bytes = decode_bytes(&params.data)?;
                let signed: SignedPaymentMethodDefine =
                    json::from_bytes(&bytes).map_err(|e| RpcError::InvalidParams(e.to_string()))?;
                let gossip_bytes =
                    wire::to_bytes(&signed).expect("SignedPaymentMethodDefine always serializes");
                let id = state
                    .payment_methods
                    .apply_define(signed)
                    .map_err(|e| RpcError::Application(e.code()))?;
                crate::dispatch::originate(
                    state,
                    protocol::EVENT_DEFINED,
                    protocol::OFS_SPEC,
                    Priority::Reputation,
                    gossip_bytes,
                );
                Ok(id.as_str().to_string())
            },
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::{encode_bytes, encode_peer_id};
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_taxonomy::{MerchantPaymentMethod, PaymentMethodCategory};
    use serde_json::json as value;

    fn table_and_state() -> (MethodTable<MemoryStore>, NodeState<MemoryStore>) {
        let mut table = MethodTable::new();
        register(&mut table);
        (table, NodeState::new_for_test(MemoryStore::new()))
    }

    fn choices(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        params: serde_json::Value,
    ) -> PaymentMethodChoices {
        let raw = table
            .dispatch(state, "getPaymentMethods", params)
            .expect("the picker read cannot fail");
        serde_json::from_value::<serde_json::Value>(raw)
            .map(|v| PaymentMethodChoices {
                country: v["country"].as_str().map(str::to_string),
                suggested: serde_json::from_value(v["suggested"].clone()).unwrap(),
                others: serde_json::from_value(v["others"].clone()).unwrap(),
                merchant: serde_json::from_value(v["merchant"].clone()).unwrap(),
            })
            .expect("the wire form deserializes back")
    }

    fn define(
        table: &MethodTable<MemoryStore>,
        state: &NodeState<MemoryStore>,
        keypair: &Keypair,
        name: &str,
    ) -> Result<String, RpcError> {
        let method = MerchantPaymentMethod {
            merchant: peer_id_from_public_key(&keypair.public_key()).unwrap(),
            merchant_public_key: keypair.public_key(),
            name: name.to_string(),
            category: PaymentMethodCategory::BankTransfer,
        };
        let signed = SignedPaymentMethodDefine::sign(method, keypair);
        let data = encode_bytes(&json::to_bytes(&signed).unwrap());
        table
            .dispatch(state, "sendPaymentMethodDefine", value!({ "data": data }))
            .map(|v| {
                v.as_str()
                    .expect("an id comes back as a string")
                    .to_string()
            })
    }

    #[test]
    fn a_merchant_in_kenya_is_offered_m_pesa_and_still_offered_everything_else() {
        let (table, state) = table_and_state();
        let answer = choices(&table, &state, value!({ "country": "KE" }));

        assert!(
            answer
                .suggested
                .iter()
                .any(|m| m.name == "M-Pesa Kenya (Safaricom)")
        );
        assert!(
            answer.others.iter().any(|m| m.name == "SEPA"),
            "a suggestion is an ordering, not a restriction"
        );
        assert_eq!(
            answer.suggested.len() + answer.others.len(),
            openfiat_taxonomy::catalog().len()
        );
        assert_eq!(answer.country.as_deref(), Some("KE"));
    }

    #[test]
    fn brazil_gets_pix_and_germany_gets_sepa() {
        let (table, state) = table_and_state();
        for (country, expected) in [("BR", "PIX"), ("DE", "SEPA")] {
            let answer = choices(&table, &state, value!({ "country": country }));
            assert!(
                answer.suggested.iter().any(|m| m.name == expected),
                "{country} must be offered {expected}"
            );
        }
    }

    /// A country this build has nothing listed for is an ordinary answer:
    /// no suggestions, the whole catalog. An error here would make a
    /// picker unusable in exactly the markets that most need one.
    #[test]
    fn a_country_with_nothing_listed_still_gets_a_usable_picker() {
        let (table, state) = table_and_state();
        for params in [value!({ "country": "AQ" }), value!({})] {
            let answer = choices(&table, &state, params);
            assert!(answer.suggested.is_empty());
            assert_eq!(answer.others.len(), openfiat_taxonomy::catalog().len());
        }
    }

    /// The whole point of the merchant-defined half: it is published
    /// once, to the node, and is readable afterwards — not written to one
    /// browser's local storage under a claim that it was shared.
    #[test]
    fn a_merchant_defines_a_rail_and_reads_it_back_from_the_node() {
        let (table, state) = table_and_state();
        let merchant = Keypair::generate();
        let id = define(&table, &state, &merchant, "Sacco Standing Order")
            .expect("a well-signed definition is accepted");

        let wallet = encode_peer_id(&peer_id_from_public_key(&merchant.public_key()).unwrap());
        let answer = choices(
            &table,
            &state,
            value!({ "country": "KE", "wallet": wallet }),
        );
        assert_eq!(answer.merchant.len(), 1);
        assert_eq!(answer.merchant[0].name, "Sacco Standing Order");
        assert_eq!(answer.merchant[0].id.as_str(), id);

        // And a counterparty holding only the id — which is all an
        // advertisement carries — can turn it back into a name.
        let resolved = table
            .dispatch(&state, "getPaymentMethod", value!({ "id": id }))
            .expect("resolving an id cannot fail");
        assert_eq!(resolved["name"], "Sacco Standing Order");
    }

    #[test]
    fn a_builtin_id_resolves_to_its_catalog_row() {
        let (table, state) = table_and_state();
        let resolved = table
            .dispatch(
                &state,
                "getPaymentMethod",
                value!({ "id": "builtin:mpesa-kenya" }),
            )
            .unwrap();
        assert_eq!(resolved["name"], "M-Pesa Kenya (Safaricom)");
        assert_eq!(resolved["category"], "MobileMoney");

        // An id nothing answers to is null, not an error: a client may be
        // reading an advertisement published by a newer node.
        assert!(
            table
                .dispatch(
                    &state,
                    "getPaymentMethod",
                    value!({ "id": "builtin:added-next-year" })
                )
                .unwrap()
                .is_null()
        );
        // A display name, though, is a client bug and is named as one.
        assert!(
            table
                .dispatch(&state, "getPaymentMethod", value!({ "id": "M-Pesa" }))
                .is_err()
        );
    }

    /// The impersonation defence, reached through the surface a merchant
    /// actually uses.
    #[test]
    fn a_look_alike_of_a_known_rail_is_refused_at_the_rpc_boundary() {
        let (table, state) = table_and_state();
        let merchant = Keypair::generate();
        for impostor in ["М-Реѕа", "M-Pesa ", "PIX", "Acme\u{202E}Pay"] {
            assert!(
                define(&table, &state, &merchant, impostor).is_err(),
                "{impostor:?} must not be publishable"
            );
        }
        let wallet = encode_peer_id(&peer_id_from_public_key(&merchant.public_key()).unwrap());
        assert!(
            choices(&table, &state, value!({ "wallet": wallet }))
                .merchant
                .is_empty()
        );
    }

    /// A definition names its author inside its own id, so one merchant
    /// cannot publish under another's prefix however they sign it.
    #[test]
    fn a_definition_is_refused_when_the_signature_is_not_the_merchants() {
        let (table, state) = table_and_state();
        let merchant = Keypair::generate();
        let impostor = Keypair::generate();
        let method = MerchantPaymentMethod {
            merchant: peer_id_from_public_key(&merchant.public_key()).unwrap(),
            merchant_public_key: merchant.public_key(),
            name: "Acme Pay".to_string(),
            category: PaymentMethodCategory::BankTransfer,
        };
        let forged = SignedPaymentMethodDefine::sign(method, &impostor);
        let data = encode_bytes(&json::to_bytes(&forged).unwrap());
        assert!(
            table
                .dispatch(&state, "sendPaymentMethodDefine", value!({ "data": data }))
                .is_err()
        );
    }
}
