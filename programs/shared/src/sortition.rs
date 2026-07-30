//! Per-case arbitrator sortition (OFS-4100 §4.1).
//!
//! # The problem this replaces
//!
//! Arbitrator seats used to go to whoever called `commit_dispute_vote`
//! first, capped at seven. Anyone holding seven staked wallets could
//! therefore take every seat on any case they chose, at the moment they
//! chose. The original OFS-4100 §4.1 claimed a large stake minimum priced
//! that out; it does not. Slashing fires for revealing *outside*
//! consensus, so an attacker holding every seat **is** the consensus and
//! is never slashed — their stake is capital locked for an unbonding
//! period, not capital at risk. A minimum is a liquidity barrier, and
//! calling a barrier a cost is what made the old claim wrong.
//!
//! # What this actually provides
//!
//! Eligibility for one specific case becomes a draw the arbitrator cannot
//! choose the outcome of: a wallet qualifies only if a hash of its stake
//! account against a per-case seed falls below a threshold. A Solana
//! program cannot enumerate accounts, so this is *self-selection with
//! verifiable eligibility* rather than the program drawing names from a
//! list — but the effect on an attacker is the same, and anyone can
//! recompute any seat's draw from public data.
//!
//! At a threshold of 1/100, expecting to qualify for a given seat needs
//! about a hundred staked wallets, each of which must independently clear
//! the role minimum and the stake-age requirement. Assembling a majority
//! of seven seats moves from "hold seven wallets" to "hold several hundred
//! aged, funded wallets" — a multiplicative barrier of capital *and* time,
//! which is what this mechanism can honestly claim.
//!
//! # Two limits, stated rather than papered over
//!
//! **Widening admits everyone eventually.** A fixed 1/100 threshold would
//! deadlock a young network: with ten registered arbitrators, the expected
//! number of qualifiers per case is 0.1, so almost every dispute would
//! reach no arbitrators at all. [`sortition_threshold_bps`] therefore
//! loosens the threshold across the commit window, reaching fully open in
//! its final slice. That is a deliberate trade: the gate *delays* seat
//! claims, it never permanently excludes anyone, so a thin arbitrator pool
//! degrades to the old first-come behaviour instead of freezing. It also
//! means an attacker who simply waits can still sweep a case that no
//! honest arbitrator is watching — against which no seat-allocation rule
//! helps.
//!
//! Widening is monotonic and is deliberately *not* frozen once a quorum is
//! seated. Freezing would look like it protects an early honest majority,
//! but it hands an attacker who wins three early seats a way to lock
//! everyone else out of the case permanently, which is strictly worse.
//!
//! Nor is the widening capped below fully open. A cap does not slow an
//! attacker down — their advantage is *ticket count*, so a threshold of
//! 64% still admits 64% of their wallets — while it does stop a small
//! honest pool from ever filling a case. It would cost liveness and buy no
//! security.
//!
//! **The seed is grindable by whoever latches it.** Solana offers no
//! in-transaction randomness. A seed derived from a recent slot hash is
//! unpredictable more than a few slots ahead — which is all the
//! stake-age requirement needs, since wallets cannot be ground into
//! existence retroactively — but the account that latches it can simulate,
//! see the resulting draw, and resubmit in a later slot until it is
//! favourable. Grinding does not remove the capital-and-time barrier: an
//! attacker still needs many aged funded wallets for any draw to land
//! well. It does mean the guarantee is "capture is expensive", not
//! "capture is impossible". Closing it properly needs either a VRF or a
//! two-transaction future-slot commit, tracked as follow-up work rather
//! than implied here.

use anchor_lang::prelude::*;
use sha2::{Digest, Sha256};

/// Basis-points denominator for a sortition threshold (10_000 = every
/// wallet qualifies), matching every other basis-point figure in this
/// workspace.
pub const SORTITION_BPS_DENOMINATOR: u32 = 10_000;

/// How many equal slices the commit window is divided into. The threshold
/// doubles at each one, and the final slice is unconditionally fully open.
///
/// Eight is chosen against the signed-off starting value of 100 bps, for
/// which doubling happens to *be* the smooth geometric ramp from 1/100 to
/// fully open: 100 → 200 → 400 → 800 → 1600 → 3200 → 6400 → 10000, the
/// last step arriving at the cap on its own.
///
/// The final slice is forced open rather than merely capped because
/// doubling alone does not get there from an arbitrary starting value —
/// from 1 bps, eight doublings reach 128, which would leave a small
/// arbitrator pool permanently unable to fill a case and turn every
/// dispute into the terminal even split. Governance can set this
/// threshold, so the schedule has to stay live for values it was not
/// tuned for.
///
/// The cost is a discontinuity in the last slice for starting values far
/// below 100 bps — from 1 bps the gate jumps 64 → 10000 — which makes the
/// final eighth of the window a pure speed race rather than a draw. That
/// is the correct direction to fail: a race is what the mechanism already
/// degrades to for a thin pool, whereas a case nobody can join freezes a
/// real escrow.
pub const SORTITION_WIDENING_STEPS: u32 = 8;

/// Domain separator, so a sortition draw can never be confused with any
/// other hash this protocol computes over a pubkey and a 32-byte seed.
const SORTITION_DOMAIN: &[u8] = b"openfiat-arbitrator-sortition";

/// This stake account's draw for this case, in basis points of
/// [`SORTITION_BPS_DENOMINATOR`] — uniform over `0..10_000`.
///
/// Keyed on the **stake account address**, not the wallet. The two are
/// equivalent here (the stake account is a PDA of the wallet and role, so
/// one wallet has exactly one arbitrator stake account and cannot hold a
/// second draw for the same case) but the stake account is what the role
/// minimum and the age clock are attached to, so drawing against it keeps
/// the identity being tested and the identity being funded the same thing.
pub fn sortition_ticket_bps(case_seed: &[u8; 32], stake_account: &Pubkey) -> u32 {
    // `sha2` rather than the SHA-256 syscall, matching what escrow's
    // commit-reveal already uses, so the two hashes in the dispute path
    // cannot drift onto different primitives.
    let mut hasher = Sha256::new();
    hasher.update(SORTITION_DOMAIN);
    hasher.update(case_seed);
    hasher.update(stake_account.as_ref());
    let digest = hasher.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    // The modulo bias here is 2^64 mod 10_000 out of 2^64 — around one
    // part in 10^15, far below anything an attacker could exploit and not
    // worth a rejection-sampling loop inside a compute-metered program.
    (u64::from_le_bytes(head) % SORTITION_BPS_DENOMINATOR as u64) as u32
}

/// The threshold in force at `now`, widening across the commit window.
///
/// `window_start`/`window_end` are the case's own `opened_at` and
/// `commit_deadline`, so a re-opened round redraws against its own fresh
/// window rather than inheriting a window that has already elapsed.
///
/// Returns fully open (`SORTITION_BPS_DENOMINATOR`) for a degenerate or
/// inverted window, and for any `initial_bps` at or above the denominator.
/// Failing open is correct for both: a broken window must not be able to
/// lock every arbitrator out of a case, which would freeze a real
/// escrow — and the shortfall path (fewer than `MIN_ARBITRATORS` counted
/// votes routing to an even split) is what bounds the damage when a case
/// genuinely cannot be filled.
pub fn sortition_threshold_bps(
    initial_bps: u16,
    window_start: i64,
    window_end: i64,
    now: i64,
) -> u32 {
    let initial = initial_bps as u32;
    if initial == 0 || initial >= SORTITION_BPS_DENOMINATOR {
        return SORTITION_BPS_DENOMINATOR;
    }
    let Some(window) = window_end.checked_sub(window_start).filter(|w| *w > 0) else {
        return SORTITION_BPS_DENOMINATOR;
    };

    let elapsed = now.saturating_sub(window_start).clamp(0, window);
    // Integer slice index, saturating at the last slice: `elapsed == window`
    // would otherwise index one past the end.
    let step = ((elapsed as i128 * SORTITION_WIDENING_STEPS as i128) / window as i128) as u32;
    let step = step.min(SORTITION_WIDENING_STEPS - 1);

    // The last slice is open to everyone regardless of where the doubling
    // has reached — see `SORTITION_WIDENING_STEPS` for why liveness has to
    // hold for thresholds the doubling was not tuned for.
    if step == SORTITION_WIDENING_STEPS - 1 {
        return SORTITION_BPS_DENOMINATOR;
    }

    initial
        .checked_shl(step)
        .unwrap_or(SORTITION_BPS_DENOMINATOR)
        .min(SORTITION_BPS_DENOMINATOR)
}

/// Whether `stake_account` may take a seat on this case at `now`.
///
/// `initial_bps` of zero disables sortition entirely, which is what makes
/// the parameter safe to ship inert and raise by governance once the
/// arbitrator pool is large enough to sustain a real draw.
pub fn qualifies_for_seat(
    case_seed: &[u8; 32],
    stake_account: &Pubkey,
    initial_bps: u16,
    window_start: i64,
    window_end: i64,
    now: i64,
) -> bool {
    if initial_bps == 0 {
        return true;
    }
    let threshold = sortition_threshold_bps(initial_bps, window_start, window_end, now);
    sortition_ticket_bps(case_seed, stake_account) < threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: i64 = 8_000;
    const START: i64 = 1_000_000;
    const END: i64 = START + WINDOW;

    #[test]
    fn a_ticket_is_stable_for_one_seed_and_account() {
        let seed = [3u8; 32];
        let key = Pubkey::new_unique();
        assert_eq!(
            sortition_ticket_bps(&seed, &key),
            sortition_ticket_bps(&seed, &key)
        );
    }

    #[test]
    fn changing_either_input_changes_the_ticket() {
        let key = Pubkey::new_unique();
        assert_ne!(
            sortition_ticket_bps(&[1u8; 32], &key),
            sortition_ticket_bps(&[2u8; 32], &key)
        );
        let seed = [9u8; 32];
        assert_ne!(
            sortition_ticket_bps(&seed, &Pubkey::new_unique()),
            sortition_ticket_bps(&seed, &Pubkey::new_unique())
        );
    }

    /// The whole mechanism rests on the draw being close to uniform: if
    /// tickets clustered, a threshold of 1/100 would admit either far more
    /// or far fewer wallets than intended, and the cost of assembling a
    /// majority would not be what §4.1 claims.
    #[test]
    fn tickets_are_roughly_uniform() {
        let seed = [42u8; 32];
        let sample = 20_000;
        let mut buckets = [0u32; 10];
        for _ in 0..sample {
            let ticket = sortition_ticket_bps(&seed, &Pubkey::new_unique());
            buckets[(ticket / 1_000) as usize] += 1;
        }
        let expected = sample / 10;
        for (index, count) in buckets.iter().enumerate() {
            let deviation = (*count as i64 - expected as i64).abs();
            assert!(
                deviation < expected as i64 / 4,
                "bucket {index} held {count}, expected around {expected}"
            );
        }
    }

    /// At the signed-off 1/100, roughly 1% of a large pool should qualify
    /// at case open. This is the number the §4.1 cost argument quotes.
    #[test]
    fn one_in_a_hundred_qualifies_at_the_opening_threshold() {
        let seed = [7u8; 32];
        let sample = 20_000;
        let qualified = (0..sample)
            .filter(|_| qualifies_for_seat(&seed, &Pubkey::new_unique(), 100, START, END, START))
            .count();
        assert!(
            (120..=280).contains(&qualified),
            "{qualified} of {sample} qualified at 1/100; expected around 200"
        );
    }

    #[test]
    fn the_threshold_doubles_each_slice_and_ends_fully_open() {
        let slice = WINDOW / SORTITION_WIDENING_STEPS as i64;
        let observed: Vec<u32> = (0..SORTITION_WIDENING_STEPS as i64)
            .map(|k| sortition_threshold_bps(100, START, END, START + k * slice))
            .collect();
        assert_eq!(
            observed,
            vec![100, 200, 400, 800, 1_600, 3_200, 6_400, 10_000]
        );
    }

    /// Liveness: whatever the starting threshold, the gate must be fully
    /// open by the deadline, or a thin arbitrator pool could never fill a
    /// case and every dispute would end in the terminal even split.
    #[test]
    fn every_starting_threshold_is_fully_open_by_the_deadline() {
        let slice = WINDOW / SORTITION_WIDENING_STEPS as i64;
        for initial in [1u16, 10, 100, 500, 2_500, 9_999] {
            // Checked across the whole final slice, not only at the exact
            // deadline: `commit_deadline` itself is already past the window
            // (`commit_dispute_vote` requires `now < commit_deadline`), so a
            // schedule that only opened on the last second would be open
            // during no callable moment at all.
            for offset in [0, slice / 2, slice - 1] {
                let at = END - slice + offset;
                assert_eq!(
                    sortition_threshold_bps(initial, START, END, at),
                    SORTITION_BPS_DENOMINATOR,
                    "initial {initial} was still gated {} seconds before the deadline",
                    END - at
                );
            }
        }
    }

    #[test]
    fn the_threshold_never_narrows_as_time_passes() {
        let mut previous = 0;
        for offset in 0..=WINDOW {
            let threshold = sortition_threshold_bps(100, START, END, START + offset);
            assert!(
                threshold >= previous,
                "threshold narrowed at offset {offset}"
            );
            previous = threshold;
        }
    }

    #[test]
    fn a_time_before_the_window_uses_the_opening_threshold() {
        assert_eq!(sortition_threshold_bps(100, START, END, START - 5_000), 100);
    }

    /// A degenerate window must fail open, not closed — see
    /// `sortition_threshold_bps`'s own doc for why a case that nobody can
    /// join is worse than one anybody can.
    #[test]
    fn a_degenerate_or_inverted_window_fails_open() {
        for (start, end) in [(START, START), (END, START)] {
            assert_eq!(
                sortition_threshold_bps(100, start, end, START),
                SORTITION_BPS_DENOMINATOR
            );
        }
    }

    #[test]
    fn a_zero_initial_threshold_disables_the_gate_entirely() {
        let seed = [11u8; 32];
        for _ in 0..500 {
            assert!(qualifies_for_seat(
                &seed,
                &Pubkey::new_unique(),
                0,
                START,
                END,
                START
            ));
        }
    }
}
