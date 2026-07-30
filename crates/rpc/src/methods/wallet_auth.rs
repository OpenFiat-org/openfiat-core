//! Proving you hold a wallet, for the reads that are not everyone's.
//!
//! # Why any read here is gated
//!
//! Nearly every method on this surface is open, because what it returns
//! is already replicated to every node. A handful are not, and the line
//! between them is not "is this secret" — nothing here is secret — but
//! "does answering this to a stranger assemble something the protocol
//! deliberately leaves scattered".
//!
//! The trade graph is that something. `methods::counterparties` makes the
//! argument in full: an endpoint that answers "who does this wallet trade
//! with, and how often" hands anyone a map of real trading relationships
//! — which merchant a wallet always returns to, who a busy merchant's
//! regulars are, and therefore who is worth following home. In a P2P fiat
//! market that is a physical-safety question rather than a preference.
//!
//! That argument was made once and enforced in one method, and the graph
//! stayed available through `getSettlements`, `getReservations` and
//! `getDisputes`, none of which took a parameter and all of which
//! returned every record on the network with both parties named. The gate
//! was not weak; it was walked around. This module exists so the check
//! lives in one place and every surface that needs it uses the same one.
//!
//! # What this is not
//!
//! It is not confidentiality. The underlying records are gossiped to
//! every node, so anyone running one can read them, and no amount of RPC
//! gating changes that. What it protects is the *ease* of the query: the
//! difference between `curl`-ing a stranger's public access node and
//! standing up a node to index the network. That difference is most of
//! what casual harvesting is made of.

use crate::dispatch::{decode_bytes, decode_peer_id, encode_peer_id};
use crate::error::RpcError;
use crate::state::NodeState;
use openfiat_crypto::verify;
use openfiat_network::identity::peer_id_from_public_key;
use openfiat_storage::KvStore;
use openfiat_types::{ErrorCode, PeerId, PublicKey, Signature, Timestamp};

/// A wallet answering a challenge: whose records, which nonce, the key
/// claiming to be that wallet, and its signature over the challenge.
#[derive(serde::Deserialize)]
pub struct WalletProof {
    /// Base64 `PeerId`, matching every other wallet-scoped method here.
    pub wallet: String,
    /// Base64 raw 32-byte Ed25519 public key. Sent explicitly rather than
    /// recovered from `wallet`, so the identity claim is something the
    /// caller states and this code checks, not something the server
    /// infers on their behalf.
    pub public_key: String,
    pub nonce: String,
    /// Base64, matching every other signed payload on this surface.
    pub signature: String,
}

/// Verifies `proof` under `domain`, returning the wallet it establishes.
///
/// # Order of checks, which is not arbitrary
///
/// The derivation check comes first, before the nonce is touched: asking
/// for a wallet you do not hold the key for is refused outright, and
/// refusing early also means a stranger's failed attempt cannot spend the
/// nonce its real owner is part-way through signing.
///
/// The nonce is then consumed *before* the signature is checked, so
/// presenting a captured signature burns the nonce rather than replaying
/// it.
///
/// # Refusal, not narrowing
///
/// A caller who cannot prove the wallet gets an error, never a filtered
/// answer. A filtering implementation looks identical in every passing
/// test right up until a refactor drops the filter; a refusal fails
/// loudly and immediately.
pub fn verify_wallet<S: KvStore + 'static>(
    state: &NodeState<S>,
    proof: &WalletProof,
    domain: &str,
) -> Result<PeerId, RpcError> {
    let wallet = decode_peer_id(&proof.wallet)?;
    let public_key = decode_public_key(&proof.public_key)?;

    let claimed = peer_id_from_public_key(&public_key)
        .map_err(|_| RpcError::Application(ErrorCode::InvalidIdentityClaim))?;
    if claimed != wallet {
        return Err(RpcError::Application(ErrorCode::InvalidIdentityClaim));
    }

    let subject = encode_peer_id(&wallet);
    let challenge = state
        .wallet_challenges
        .borrow_mut()
        .consume(&subject, &proof.nonce, Timestamp::now())
        .map_err(|e| RpcError::Application(e.code()))?;

    let raw: [u8; 64] = decode_bytes(&proof.signature)?
        .try_into()
        .map_err(|_| RpcError::InvalidParams("signature must be 64 bytes".into()))?;
    verify(
        &public_key,
        // The domain separator is why a signature collected for one
        // gated surface cannot be presented on another, even though both
        // identify their subject by the same base64 peer id and draw
        // nonces from the same ledger.
        &challenge.signing_bytes(domain),
        &Signature::from_bytes(raw),
    )
    .map_err(|e| RpcError::Application(e.code()))?;

    Ok(wallet)
}

pub fn decode_public_key(encoded: &str) -> Result<PublicKey, RpcError> {
    let raw: [u8; 32] = decode_bytes(encoded)?
        .try_into()
        .map_err(|_| RpcError::InvalidParams("public key must be 32 bytes".into()))?;
    Ok(PublicKey::from_bytes(raw))
}

/// Hands out a nonce for `wallet` to sign.
///
/// Deliberately open. A nonce is worthless without the private key that
/// signs it, and demanding a signature to obtain the thing you sign would
/// be circular. It confirms nothing about the wallet, not even that it
/// exists.
///
/// One issuer for every gated surface, because a nonce carries no domain:
/// the separation is in what the caller signs, so the same nonce answers
/// `getMySettlements` or `getCounterparties` depending on which domain
/// the signature was made under, and can answer exactly one of them once.
pub fn register<S: KvStore + 'static>(table: &mut crate::dispatch::MethodTable<S>) {
    table.register(
        "getWalletChallenge",
        crate::dispatch::method_fn(
            |state: &NodeState<S>,
             params: crate::dispatch::WalletParams|
             -> Result<openfiat_crypto::challenge::Challenge, RpcError> {
                // Normalized through `PeerId` so the subject the caller
                // signs is the canonical encoding of their wallet, not
                // whichever base64 spelling they happened to send.
                let wallet = encode_peer_id(&decode_peer_id(&params.wallet)?);
                Ok(state.wallet_challenges.borrow_mut().issue(
                    wallet,
                    Timestamp::now(),
                    openfiat_crypto::challenge::CHALLENGE_TTL_SECS,
                ))
            },
        ),
    );
}
