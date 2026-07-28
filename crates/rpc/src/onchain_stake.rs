//! Hand-rolled decoder for the on-chain `openfiat-staking` program's
//! `StakeAccount` (OFS-4200 §5), read via `ChainClient::get_account`.
//!
//! `openfiat-core/programs` is a deliberately separate Cargo/Anchor
//! workspace from this one (see `programs/README.md`'s own rationale —
//! it pins its own Solana SDK versions), so this crate can't just import
//! `staking::StakeAccount` and let Borsh deserialize it. Instead this
//! decodes the account's raw bytes directly, using the exact layout and
//! 8-byte Anchor discriminator taken from a real `anchor build`'s
//! generated `programs/target/idl/staking.json` — not recomputed by
//! hand, so it can't drift from what the deployed program actually
//! writes.
//!
//! This is the trust anchor for `crates/rpc::actor::poll_vote_verifications`:
//! a governance vote's self-reported weight is never trusted directly
//! (see `crates/governance`'s own now-superseded placeholder) — only the
//! `amount` decoded here, from an account independently confirmed to be
//! both owned by the staking program and to actually belong to the
//! voter, is.

use crate::error::RpcError;

/// `sha256("account:StakeAccount")[..8]`, as Anchor itself computed it —
/// taken verbatim from `programs/target/idl/staking.json`.
const STAKE_ACCOUNT_DISCRIMINATOR: [u8; 8] = [80, 158, 67, 124, 50, 189, 192, 255];

/// The two `StakeAccount` fields a vote-weight check needs. Everything
/// after `amount` (unbonding state, slash history, pending rewards) is
/// irrelevant here and deliberately left undecoded.
pub struct DecodedStakeAccount {
    /// The wallet this stake belongs to, raw Ed25519 bytes — compared
    /// directly against a vote's claimed `voter_public_key`, sidestepping
    /// any need for a base58 encoder in this crate.
    pub owner: [u8; 32],
    /// Currently-staked amount (excludes anything mid-unbonding) — this
    /// is OFS-4200 §5's `get_effective_stake`, and becomes a verified
    /// vote's real weight.
    pub amount: u64,
}

/// Byte offsets within a `StakeAccount` account's data, following
/// `openfiat-staking::state::StakeAccount`'s field order exactly:
/// discriminator(8) + owner(32) + role(1) + amount(8) + ...
const OWNER_OFFSET: usize = 8;
const AMOUNT_OFFSET: usize = OWNER_OFFSET + 32 + 1;
const MIN_LEN: usize = AMOUNT_OFFSET + 8;

pub fn decode_stake_account(data: &[u8]) -> Result<DecodedStakeAccount, RpcError> {
    if data.len() < MIN_LEN {
        return Err(RpcError::Application(
            openfiat_types::ErrorCode::MalformedTransaction,
        ));
    }
    if data[..8] != STAKE_ACCOUNT_DISCRIMINATOR {
        return Err(RpcError::Application(
            openfiat_types::ErrorCode::MalformedTransaction,
        ));
    }

    let mut owner = [0u8; 32];
    owner.copy_from_slice(&data[OWNER_OFFSET..OWNER_OFFSET + 32]);
    let amount = u64::from_le_bytes(
        data[AMOUNT_OFFSET..AMOUNT_OFFSET + 8]
            .try_into()
            .expect("slice is exactly 8 bytes"),
    );

    Ok(DecodedStakeAccount { owner, amount })
}

/// A well-formed `StakeAccount`'s raw bytes, for tests elsewhere in this
/// crate (`actor::poll_vote_verifications`'s own tests) that need a fake
/// `ChainClient::get_account` response without a real cluster.
#[cfg(test)]
pub(crate) fn fixture_stake_account_bytes(owner: [u8; 32], amount: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&STAKE_ACCOUNT_DISCRIMINATOR);
    bytes.extend_from_slice(&owner);
    bytes.push(2); // role: NodeOperator, irrelevant to this decoder
    bytes.extend_from_slice(&amount.to_le_bytes());
    // Trailing fields (unbonding_amount, unbonding_release_at,
    // slashed_total, pending_rewards, bump) — irrelevant, omitted.
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_owner_and_amount_from_a_well_formed_account() {
        let owner = [7u8; 32];
        let decoded = decode_stake_account(&fixture_stake_account_bytes(owner, 12_345)).unwrap();
        assert_eq!(decoded.owner, owner);
        assert_eq!(decoded.amount, 12_345);
    }

    #[test]
    fn rejects_a_mismatched_discriminator() {
        let mut bytes = fixture_stake_account_bytes([1u8; 32], 100);
        bytes[0] ^= 0xFF;
        assert!(decode_stake_account(&bytes).is_err());
    }

    #[test]
    fn rejects_data_shorter_than_the_minimum_layout() {
        assert!(decode_stake_account(&STAKE_ACCOUNT_DISCRIMINATOR).is_err());
    }
}
