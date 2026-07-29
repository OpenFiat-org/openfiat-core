//! Proving control of a key without a login: single-use, expiring
//! sign-this-nonce challenges.
//!
//! There are no accounts in this protocol, so "is this really you?" can
//! only ever be answered by asking the caller to sign something they
//! could not have signed in advance. A subject asks for a challenge,
//! signs the exact bytes it names, and presents the signature; the
//! verifier checks it against the public key the subject is claiming to
//! be. Nothing is stored about the caller, and a signature captured off
//! the wire buys an attacker nothing because the nonce it authorises is
//! spent on first presentation.
//!
//! # Why the ledger is keyed by nonce, not by subject
//!
//! Issuing has to be open — a nonce is worthless without the private key
//! that signs it, and demanding a signature to obtain the thing you sign
//! would be circular. That makes issuance an anonymous write, so it must
//! not be able to disturb anybody else's in-flight challenge. Keying by
//! subject would let a stranger request challenges for your subject in a
//! loop and invalidate the one you are part-way through signing, locking
//! you out for as long as they cared to keep it up. Keying by nonce
//! removes that lever entirely: your outstanding challenge is reachable
//! only by someone who already knows its random 32 bytes.
//!
//! The cost is that unanswered challenges accumulate rather than
//! replacing each other, so [`ChallengeLedger::issue`] prunes expired
//! entries and enforces [`ChallengeLedger::MAX_OUTSTANDING`].
//!
//! `openfiat_registry::earnings::EarningsLedger` used to implement this
//! handshake independently, keyed by subject, and has been migrated onto
//! this module. It keeps its own `EarningsChallenge` wire type — released
//! SDKs name the field `service_id` rather than `subject` — but the
//! storage is this ledger and its signing bytes are produced by
//! [`Challenge::signing_bytes`] under the `openfiat-earnings` domain, so
//! the two cannot drift into disagreeing about what a provider signs.
//!
//! That migration was the fix for a live vulnerability, not a tidy-up: while
//! it keyed by subject, any caller could request challenges for somebody
//! else's `service_id` in a loop and keep invalidating the one that provider
//! was part-way through signing, denying it `getProviderEarnings`
//! indefinitely without credentials, stake, or any relationship to the
//! service.

use openfiat_types::{ErrorCode, Timestamp};
use rand::rngs::{StdRng, SysRng};
use rand::{RngExt, SeedableRng};
use std::collections::HashMap;

/// How long an unanswered challenge stays valid: long enough for a human
/// to read and approve a wallet prompt, short enough that an unspent
/// nonce is not left lying around.
pub const CHALLENGE_TTL_SECS: u64 = 300;

/// Why a presented challenge was not accepted.
///
/// Deliberately does not distinguish "never existed" from "already
/// spent" — both are [`Self::Unknown`], because telling a caller which
/// one it was would confirm that some other party is mid-handshake for
/// that subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeError {
    Unknown,
    Expired,
}

impl ChallengeError {
    /// The OFS-8000 code this failure maps to.
    pub const fn code(self) -> ErrorCode {
        match self {
            Self::Unknown => ErrorCode::ResourceNotFound,
            Self::Expired => ErrorCode::InvalidRequest,
        }
    }
}

/// A single-use, expiring challenge bound to one subject.
///
/// `subject` is whatever string identifies the thing control is being
/// proved over — a wallet, a service id — in the exact encoding the
/// verifier will compare against, since it is signed verbatim.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Challenge {
    pub subject: String,
    /// 32 random bytes, hex-encoded so it survives JSON unchanged.
    pub nonce: String,
    pub expires_at: Timestamp,
}

impl Challenge {
    /// The exact bytes the subject must sign.
    ///
    /// `domain` separates one feature's challenges from another's, so a
    /// signature collected for, say, reading a statement can never be
    /// replayed against a different surface that happens to share a
    /// subject namespace.
    pub fn signing_bytes(&self, domain: &str) -> Vec<u8> {
        format!("{domain}:{}:{}", self.subject, self.nonce).into_bytes()
    }
}

/// The outstanding challenges a verifier has issued and not yet seen
/// answered.
///
/// In-memory on purpose: an unanswered challenge is worthless after a
/// restart, and persisting one would hand a node operator a record of
/// who asked what and when — exactly the kind of harvestable trail these
/// challenges exist to avoid creating.
#[derive(Default)]
pub struct ChallengeLedger {
    /// Keyed by nonce — see the module docs for why not by subject.
    outstanding: HashMap<String, Challenge>,
}

impl ChallengeLedger {
    /// Ceiling on unanswered challenges held at once. Reached only under
    /// deliberate flooding; the oldest-expiring entries are dropped
    /// first, so a caller who signs promptly is unaffected.
    pub const MAX_OUTSTANDING: usize = 4_096;

    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh challenge for `subject`, valid for `ttl_secs`.
    ///
    /// Existing challenges — for this subject or any other — are left
    /// alone, so one caller can never invalidate another's.
    pub fn issue(
        &mut self,
        subject: impl Into<String>,
        now: Timestamp,
        ttl_secs: u64,
    ) -> Challenge {
        self.prune(now);
        // Same OS-entropy pattern `Keypair::generate` uses: `SysRng` is
        // fallible-only, so it seeds an infallible `StdRng` once.
        let mut rng = StdRng::try_from_rng(&mut SysRng).expect("OS entropy source unavailable");
        let nonce: [u8; 32] = rng.random();
        let challenge = Challenge {
            subject: subject.into(),
            nonce: nonce.iter().map(|b| format!("{b:02x}")).collect(),
            expires_at: Timestamp::from_millis(now.as_millis().saturating_add(ttl_secs * 1_000)),
        };
        self.outstanding
            .insert(challenge.nonce.clone(), challenge.clone());
        challenge
    }

    /// Consume the challenge `nonce` names, if it exists, has not
    /// expired, and was issued for `subject`.
    ///
    /// Consumption happens whether or not the caller's signature later
    /// verifies, which is what makes a captured signature useless: the
    /// nonce it authorises is spent on first presentation.
    pub fn consume(
        &mut self,
        subject: &str,
        nonce: &str,
        now: Timestamp,
    ) -> Result<Challenge, ChallengeError> {
        let challenge = self
            .outstanding
            .remove(nonce)
            .ok_or(ChallengeError::Unknown)?;
        // A nonce issued for one subject must not open another, even
        // before the signature is looked at.
        if challenge.subject != subject {
            return Err(ChallengeError::Unknown);
        }
        if challenge.expires_at.as_millis() < now.as_millis() {
            return Err(ChallengeError::Expired);
        }
        Ok(challenge)
    }

    /// How many challenges are currently held. Diagnostics only.
    pub fn outstanding(&self) -> usize {
        self.outstanding.len()
    }

    /// Drop everything already expired, then enforce the ceiling by
    /// dropping whatever expires soonest.
    fn prune(&mut self, now: Timestamp) {
        self.outstanding
            .retain(|_, challenge| challenge.expires_at.as_millis() >= now.as_millis());
        if self.outstanding.len() < Self::MAX_OUTSTANDING {
            return;
        }
        let mut by_expiry: Vec<(String, Timestamp)> = self
            .outstanding
            .iter()
            .map(|(nonce, challenge)| (nonce.clone(), challenge.expires_at))
            .collect();
        by_expiry.sort_unstable_by_key(|(_, expires_at)| *expires_at);
        let excess = self.outstanding.len() + 1 - Self::MAX_OUTSTANDING;
        for (nonce, _) in by_expiry.into_iter().take(excess) {
            self.outstanding.remove(&nonce);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOMAIN: &str = "openfiat-test";

    fn now() -> Timestamp {
        Timestamp::from_millis(1_000)
    }

    #[test]
    fn a_challenge_is_single_use() {
        let mut ledger = ChallengeLedger::new();
        let challenge = ledger.issue("alice", now(), CHALLENGE_TTL_SECS);

        assert!(ledger.consume("alice", &challenge.nonce, now()).is_ok());
        assert_eq!(
            ledger.consume("alice", &challenge.nonce, now()),
            Err(ChallengeError::Unknown),
            "replaying a spent nonce must fail"
        );
    }

    #[test]
    fn an_expired_challenge_is_rejected() {
        let mut ledger = ChallengeLedger::new();
        let challenge = ledger.issue("alice", now(), CHALLENGE_TTL_SECS);
        let too_late = Timestamp::from_millis(now().as_millis() + CHALLENGE_TTL_SECS * 1_000 + 1);

        assert_eq!(
            ledger.consume("alice", &challenge.nonce, too_late),
            Err(ChallengeError::Expired)
        );
    }

    #[test]
    fn a_nonce_issued_for_one_subject_does_not_open_another() {
        let mut ledger = ChallengeLedger::new();
        let mine = ledger.issue("alice", now(), CHALLENGE_TTL_SECS);

        assert_eq!(
            ledger.consume("bob", &mine.nonce, now()),
            Err(ChallengeError::Unknown)
        );
    }

    /// The property keying by nonce exists to give: an anonymous caller
    /// requesting challenges for someone else's subject must not be able
    /// to invalidate the one that subject is part-way through signing.
    #[test]
    fn a_stranger_issuing_for_your_subject_cannot_invalidate_your_challenge() {
        let mut ledger = ChallengeLedger::new();
        let mine = ledger.issue("alice", now(), CHALLENGE_TTL_SECS);
        for _ in 0..16 {
            ledger.issue("alice", now(), CHALLENGE_TTL_SECS);
        }

        assert!(
            ledger.consume("alice", &mine.nonce, now()).is_ok(),
            "the challenge Alice is signing must survive a flood of issuances for her subject"
        );
    }

    #[test]
    fn two_challenges_never_repeat_a_nonce() {
        let mut ledger = ChallengeLedger::new();
        let first = ledger.issue("alice", now(), CHALLENGE_TTL_SECS).nonce;
        let second = ledger.issue("alice", now(), CHALLENGE_TTL_SECS).nonce;
        assert_ne!(first, second);
        assert_eq!(first.len(), 64, "32 random bytes, hex-encoded");
    }

    #[test]
    fn signing_bytes_are_separated_by_domain_and_subject() {
        let a = Challenge {
            subject: "alice".to_string(),
            nonce: "ff".to_string(),
            expires_at: now(),
        };
        let b = Challenge {
            subject: "bob".to_string(),
            ..a.clone()
        };
        assert_ne!(a.signing_bytes(DOMAIN), b.signing_bytes(DOMAIN));
        assert_ne!(a.signing_bytes(DOMAIN), a.signing_bytes("other-domain"));
    }

    #[test]
    fn expired_challenges_do_not_accumulate() {
        let mut ledger = ChallengeLedger::new();
        for _ in 0..8 {
            ledger.issue("alice", now(), CHALLENGE_TTL_SECS);
        }
        assert_eq!(ledger.outstanding(), 8);

        let after_ttl = Timestamp::from_millis(now().as_millis() + CHALLENGE_TTL_SECS * 1_000 + 1);
        ledger.issue("alice", after_ttl, CHALLENGE_TTL_SECS);
        assert_eq!(
            ledger.outstanding(),
            1,
            "issuing must sweep everything already expired"
        );
    }

    #[test]
    fn the_outstanding_ceiling_is_enforced() {
        let mut ledger = ChallengeLedger::new();
        for _ in 0..ChallengeLedger::MAX_OUTSTANDING + 64 {
            ledger.issue("flood", now(), CHALLENGE_TTL_SECS);
        }
        assert!(ledger.outstanding() <= ChallengeLedger::MAX_OUTSTANDING);
    }
}
