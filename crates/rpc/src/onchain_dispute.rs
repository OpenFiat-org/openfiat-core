//! Hand-rolled decoder for the on-chain `openfiat-escrow` program's
//! `DisputeCase` (OFS-4200 §4b), read via `ChainClient::get_account`.
//!
//! Same arrangement as [`crate::onchain_stake`] and for the same reason:
//! `openfiat-core/programs` is a separate Cargo/Anchor workspace pinning
//! its own Solana SDK versions, so this crate cannot import
//! `escrow::DisputeCase` and let Borsh do the work. The layout and the
//! 8-byte discriminator below are taken verbatim from a real
//! `anchor build`'s `programs/target/idl/escrow.json`, so they cannot
//! drift from what the deployed program writes.
//!
//! # Why the node reads this at all
//!
//! The off-chain dispute registry used to tally reveals itself and
//! declare a winner. The chain re-arbitrates the same case under
//! different rules — stake-weighted, with a quorum floor, re-opening a
//! round on a tie rather than resolving it — so the two could reach
//! different answers about the same dispute, and the interface would show
//! the off-chain one while the money followed the chain's.
//!
//! It does not tally any more. `DisputeRegistry::apply_onchain_execution`
//! records what the chain decided, and this is where that answer is read
//! from: the case account's own `outcome`, after the transaction that set
//! it has been independently observed as confirmed.

use crate::error::RpcError;
use openfiat_disputes::Resolution;

/// `sha256("account:DisputeCase")[..8]`, as Anchor computed it — taken
/// verbatim from `programs/target/idl/escrow.json`.
///
/// Kept beside the offsets it introduces rather than in
/// `openfiat_chain::programs`: a discriminator is only meaningful with
/// the layout that follows it, and the two must change together.
const DISPUTE_CASE_DISCRIMINATOR: [u8; 8] = [164, 200, 54, 239, 94, 76, 51, 130];

/// Fixed-size prefix before the first `Vec`, in declaration order:
/// `reservation_id` u64, `trade_escrow` pubkey, `opened_at`,
/// `commit_deadline`, `reveal_deadline` i64s, `resolved` bool, `round`
/// u8, `commit_window_secs`, `reveal_window_secs` i64s, `case_seed`
/// `[u8; 32]`, `round_opened_at` i64.
const FIXED_PREFIX: usize = 8 + 32 + 8 + 8 + 8 + 1 + 1 + 8 + 8 + 32 + 8;

/// What the chain decided, if it has decided.
///
/// `None` means the case exists and is still running — a round that has
/// not been executed, or one that re-opened. That is a real answer and
/// distinct from "no such case", which is why this is nested rather than
/// flattened into the outer `Option`.
pub fn decode_outcome(owner: &str, data: &[u8]) -> Result<Option<Resolution>, RpcError> {
    // The account must belong to *the* escrow program. Without this a
    // node would read an account somebody else wrote at an address they
    // chose, and believe whatever outcome it contained — the same
    // failure `onchain_stake` guards against for vote weight.
    if owner != openfiat_chain::PROGRAM_IDS.escrow {
        return Err(RpcError::InvalidParams(
            "dispute case account is not owned by the escrow program".into(),
        ));
    }
    let mut cursor = Cursor::new(data);
    if cursor.take(8)? != DISPUTE_CASE_DISCRIMINATOR {
        return Err(RpcError::InvalidParams(
            "account is not a DisputeCase".into(),
        ));
    }
    cursor.take(FIXED_PREFIX)?;

    // Five vectors sit between the prefix and the outcome. Each is
    // skipped by its own length rather than by a guessed arbitrator
    // count, because the counts genuinely differ per case and a
    // hard-coded seat number would decode the wrong bytes on any case
    // that did not fill exactly that many seats.
    cursor.skip_vec(32)?; // arbitrators: Vec<Pubkey>
    cursor.skip_vec(32)?; // commitments: Vec<[u8; 32]>
    cursor.skip_option_vec()?; // revealed_outcomes: Vec<Option<DisputeOutcome>>
    cursor.skip_vec(8)?; // weights: Vec<u64>
    cursor.skip_vec(1)?; // reward_claimed: Vec<bool>
    // Seats retired for committing in an earlier round and never
    // revealing (#123). Missing here, the outcome tag was read
    // `4 + 32 * barred.len()` bytes early — harmless only while the
    // vector was empty, and reading arbitrator-chosen pubkey bytes as a
    // verdict the moment a round re-opened.
    cursor.skip_vec(32)?; // barred: Vec<Pubkey>

    // deposit_vault, deposit_mint, deposit, deposit_shortfall.
    // `deposit_shortfall` was also missing, for another 8 bytes.
    cursor.take(32 + 32 + 8 + 8)?;

    match cursor.take(1)?[0] {
        0 => Ok(None),
        1 => Ok(Some(resolution_from_discriminant(cursor.take(1)?[0])?)),
        _ => Err(RpcError::InvalidParams(
            "outcome is neither present nor absent".into(),
        )),
    }
}

/// The program's `DisputeOutcome` onto this workspace's `Resolution`.
///
/// Written out rather than cast, because the two enums are declared in
/// different workspaces and nothing but this function stops them drifting
/// apart. `MutualSettlement` has no `Resolution` counterpart — OFS-2400
/// reaches it by the parties agreeing rather than by an arbitrator ruling
/// — and is reported as an error rather than folded into `Invalid`, which
/// pays a different party.
fn resolution_from_discriminant(raw: u8) -> Result<Resolution, RpcError> {
    match raw {
        0 => Ok(Resolution::BuyerWins),
        1 => Ok(Resolution::MerchantWins),
        2 => Err(RpcError::InvalidParams(
            "the chain settled this case by mutual agreement, which is not an arbitrator ruling"
                .into(),
        )),
        3 => Ok(Resolution::Invalid),
        _ => Err(RpcError::InvalidParams(
            "unknown DisputeOutcome discriminant".into(),
        )),
    }
}

/// A bounds-checked reader over account bytes that came from the network.
struct Cursor<'a> {
    data: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], RpcError> {
        if self.data.len() < count {
            return Err(RpcError::InvalidParams(
                "dispute case account is shorter than its own layout".into(),
            ));
        }
        let (taken, rest) = self.data.split_at(count);
        self.data = rest;
        Ok(taken)
    }

    /// Borsh vector: a u32 length, then `len × item_size` bytes.
    fn skip_vec(&mut self, item_size: usize) -> Result<(), RpcError> {
        let len = u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .expect("take(4) returns four bytes"),
        ) as usize;
        // Multiplied in a wider type: a length near u32::MAX times an
        // item size overflows `usize` on a 32-bit target and would wrap
        // to a small number, skipping far too little and decoding
        // whatever followed as an outcome.
        let bytes = (len as u64)
            .checked_mul(item_size as u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| RpcError::InvalidParams("implausible vector length".into()))?;
        self.take(bytes).map(|_| ())
    }

    /// `Vec<Option<T>>` where `T` is a one-byte enum: each element is a
    /// presence byte plus, when present, the discriminant. Variable per
    /// element, so it cannot be skipped by multiplication.
    fn skip_option_vec(&mut self) -> Result<(), RpcError> {
        let len = u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .expect("take(4) returns four bytes"),
        ) as usize;
        for _ in 0..len {
            if self.take(1)?[0] == 1 {
                self.take(1)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an account body the way the program would write one.
    fn account(seats: usize, outcome: Option<u8>) -> Vec<u8> {
        let mut bytes = DISPUTE_CASE_DISCRIMINATOR.to_vec();
        bytes.extend(std::iter::repeat_n(0u8, FIXED_PREFIX));
        for item in [32usize, 32] {
            bytes.extend((seats as u32).to_le_bytes());
            bytes.extend(std::iter::repeat_n(7u8, seats * item));
        }
        // revealed_outcomes: every seat revealed, so a presence byte and
        // a discriminant each.
        bytes.extend((seats as u32).to_le_bytes());
        for _ in 0..seats {
            bytes.extend([1u8, 0u8]);
        }
        for item in [8usize, 1] {
            bytes.extend((seats as u32).to_le_bytes());
            bytes.extend(std::iter::repeat_n(3u8, seats * item));
        }
        // barred: Vec<Pubkey>. Deliberately non-empty, and deliberately
        // filled with 0x01 rather than zeroes. An empty `barred` is
        // exactly what hid the missing skip for as long as it was
        // missing: the misread landed on a high byte of `deposit`, which
        // is zero for any realistic deposit, so it decoded as "no outcome
        // yet" and failed safe by luck. A fixture that cannot reproduce
        // the failure is not a fixture for it, and 0x01 is the byte that
        // decodes as "an outcome is present" — the value an arbitrator
        // would need in their own pubkey to forge one.
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(std::iter::repeat_n(0x01u8, 32));
        // deposit_vault, deposit_mint, deposit, deposit_shortfall
        bytes.extend(std::iter::repeat_n(9u8, 32 + 32 + 8 + 8));
        match outcome {
            Some(discriminant) => bytes.extend([1u8, discriminant]),
            None => bytes.push(0),
        }
        bytes
    }

    fn escrow() -> &'static str {
        openfiat_chain::PROGRAM_IDS.escrow
    }

    #[test]
    fn reads_the_outcome_the_chain_recorded() {
        assert_eq!(
            decode_outcome(escrow(), &account(3, Some(0))).unwrap(),
            Some(Resolution::BuyerWins)
        );
        assert_eq!(
            decode_outcome(escrow(), &account(5, Some(1))).unwrap(),
            Some(Resolution::MerchantWins)
        );
        assert_eq!(
            decode_outcome(escrow(), &account(7, Some(3))).unwrap(),
            Some(Resolution::Invalid)
        );
    }

    #[test]
    fn a_running_case_has_no_outcome_rather_than_a_default_one() {
        // The distinction this whole decoder exists for: "not decided
        // yet" must never arrive as a verdict.
        assert_eq!(decode_outcome(escrow(), &account(3, None)).unwrap(), None);
    }

    #[test]
    fn the_seat_count_does_not_have_to_be_guessed() {
        // Every vector is skipped by its own recorded length. A case with
        // one seat and a case with seven decode the same field.
        for seats in [0, 1, 3, 7] {
            assert_eq!(
                decode_outcome(escrow(), &account(seats, Some(1))).unwrap(),
                Some(Resolution::MerchantWins),
                "{seats} seats"
            );
        }
    }

    #[test]
    fn an_account_owned_by_another_program_is_refused() {
        // Without this a node reads an account somebody else wrote at an
        // address they chose, and believes the outcome in it.
        let impostor = openfiat_chain::PROGRAM_IDS.staking;
        assert!(decode_outcome(impostor, &account(3, Some(0))).is_err());
    }

    #[test]
    fn a_mutual_settlement_is_not_reported_as_an_arbitrator_ruling() {
        // It has no `Resolution` counterpart, and folding it into
        // `Invalid` would report an agreement between the parties as a
        // ruling that pays a different one.
        assert!(decode_outcome(escrow(), &account(3, Some(2))).is_err());
    }

    #[test]
    fn a_truncated_account_is_refused_rather_than_read_past() {
        let full = account(3, Some(0));
        for cut in [0, 8, FIXED_PREFIX, full.len() - 1] {
            assert!(
                decode_outcome(escrow(), &full[..cut]).is_err(),
                "{cut} bytes must not decode"
            );
        }
    }

    #[test]
    fn an_implausible_vector_length_is_refused_rather_than_wrapping() {
        let mut bytes = DISPUTE_CASE_DISCRIMINATOR.to_vec();
        bytes.extend(std::iter::repeat_n(0u8, FIXED_PREFIX));
        bytes.extend(u32::MAX.to_le_bytes());
        assert!(decode_outcome(escrow(), &bytes).is_err());
    }
}
