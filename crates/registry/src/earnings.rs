//! What a service has earned, and who is allowed to read it
//! (OFS-4100 §9.5).
//!
//! # Nothing credits this ledger yet
//!
//! This is a working ledger that currently reads empty for every service.
//! §9.5 settles the *mechanism* — a declared price, a registered payout
//! wallet, a signature-authenticated statement — and the per-role billing
//! triggers stand as follows:
//!
//! - **Notification gateway**: the participant who enabled notifications
//!   pays, per delivery. The one role with an identifiable beneficiary, so
//!   collection is clearly definable. Still `[PROPOSED — NEEDS SIGN-OFF]`,
//!   and not yet metered — this is the seam [`EarningsLedger::credit`]
//!   exists for.
//! - **Oracle provider**: reads are **free, by decision**. Not pending,
//!   not unimplemented.
//! - **Snapshot provider**: downloads are **free, by decision**. Likewise.
//! - **Risk intelligence**: open.
//!
//! Oracle rates and snapshots are free because charging for them would
//! work against the protocol. A priced rate feed is consulted less, which
//! makes the median it contributes to thinner and easier to move; a priced
//! snapshot slows the thing that lets a new node join at all. Both are
//! load-bearing public goods. Do not "finish" them by adding a meter.
//!
//! The consequence, so a provider reads it here rather than discovering it
//! after staking: oracle and snapshot providers have **no direct revenue**.
//! They are not paid a protocol reward and their service is free. Both are
//! usually run by parties already operating a node, so compensation arrives
//! through the node reward pool (§9.2) and the marginal cost of also
//! publishing rates or serving snapshots is small. Running one standalone
//! earns nothing.
//!
//! # Why a challenge rather than a session
//!
//! There is no provider login and no account to log into — only a key that
//! can demonstrate control of a Service ID already on file. A provider
//! asks for a challenge, signs it, and reads their statement back. The
//! challenge is random, bound to one Service ID, single-use and expiring,
//! so a signature captured off the wire buys an attacker nothing: the
//! nonce it authorises is already spent.

use crate::error::RegistryError;
use openfiat_crypto::challenge::{Challenge, ChallengeError, ChallengeLedger};
use openfiat_types::{Amount, ServiceId, Timestamp};
use std::collections::HashMap;

/// How long an unanswered challenge stays valid.
///
/// Long enough for a human to approve a wallet prompt, short enough that
/// an unspent nonce is not left lying around. `[PROPOSED — NEEDS
/// SIGN-OFF]`, like every other unsigned-off parameter here.
pub const CHALLENGE_TTL_SECS: u64 = 300;

/// One credit to a service, in whichever token the provider bills in.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EarningEntry {
    /// Base58 SPL mint the credit is denominated in.
    pub token_mint: String,
    pub amount: Amount,
    /// Free-form provenance — which trade, which delivery, which epoch.
    /// Opaque here because only the crediting role knows what identifies
    /// its own unit of work.
    pub reference: String,
    pub credited_at: Timestamp,
}

/// A service's earnings statement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderEarnings {
    pub service_id: ServiceId,
    /// Where these funds are payable, as declared on the registration.
    /// `None` means the provider never declared one, in which case
    /// nothing could be paid out even once crediting exists.
    pub payout_wallet: Option<String>,
    pub entries: Vec<EarningEntry>,
}

impl ProviderEarnings {
    /// Totals per token mint. A provider may bill different services, or
    /// different units, in different tokens, so a single scalar total
    /// would be meaningless.
    pub fn totals(&self) -> Vec<(String, Amount)> {
        let mut totals: Vec<(String, Amount)> = Vec::new();
        for entry in &self.entries {
            match totals
                .iter_mut()
                .find(|(mint, _)| *mint == entry.token_mint)
            {
                Some((_, running)) => {
                    if let Some(sum) = running.checked_add(entry.amount) {
                        *running = sum;
                    }
                }
                None => totals.push((entry.token_mint.clone(), entry.amount)),
            }
        }
        totals
    }
}

/// The domain separator for this handshake's signing bytes.
///
/// Held as a constant rather than inlined because it is now the one thing
/// tying [`EarningsChallenge`] to the generic
/// [`openfiat_crypto::challenge::Challenge`] underneath it: the wire bytes
/// are produced by that type, and this names which handshake they belong
/// to. Changing it invalidates every signature released SDKs produce.
const SIGNING_DOMAIN: &str = "openfiat-earnings";

/// A single-use, expiring challenge bound to one Service ID.
///
/// Kept as its own wire type rather than exposing
/// [`openfiat_crypto::challenge::Challenge`] directly: this shape is
/// already consumed by released SDKs, which name the field `service_id`
/// rather than `subject`. The storage underneath is the shared ledger; only
/// the serialized form is local.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EarningsChallenge {
    pub service_id: ServiceId,
    /// 32 random bytes, hex-encoded so it survives JSON unchanged.
    pub nonce: String,
    pub expires_at: Timestamp,
}

impl EarningsChallenge {
    /// The exact bytes a provider signs. Includes the Service ID, so a
    /// nonce issued for one service cannot be replayed against another
    /// even before single-use consumption.
    ///
    /// Delegates to the shared primitive rather than formatting the string
    /// here, so the two cannot drift into producing different bytes for the
    /// same challenge — which would be silent and would break every signer.
    pub fn signing_bytes(&self) -> Vec<u8> {
        self.as_challenge().signing_bytes(SIGNING_DOMAIN)
    }

    fn as_challenge(&self) -> Challenge {
        Challenge {
            subject: self.service_id.as_str().to_string(),
            nonce: self.nonce.clone(),
            expires_at: self.expires_at,
        }
    }

    fn from_challenge(challenge: Challenge, service_id: &ServiceId) -> Self {
        Self {
            service_id: service_id.clone(),
            nonce: challenge.nonce,
            expires_at: challenge.expires_at,
        }
    }
}

/// In-memory ledger plus the outstanding challenges against it.
///
/// Deliberately not persisted: an unanswered challenge is worthless after
/// a restart, and the entries are empty until a metering path exists to
/// fill them. When crediting arrives the entries will need a `KvStore`
/// the way every other registry here has one — the seam is
/// [`Self::credit`], not the storage decision.
#[derive(Default)]
pub struct EarningsLedger {
    entries: HashMap<String, Vec<EarningEntry>>,
    challenges: ChallengeLedger,
}

impl EarningsLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Credit a service. Nothing calls this yet.
    ///
    /// The caller this is waiting for is notification delivery. Oracle and
    /// snapshot work is free by decision and will never call it — see the
    /// module docs before adding a meter to either.
    pub fn credit(&mut self, service_id: &ServiceId, entry: EarningEntry) {
        self.entries
            .entry(service_id.as_str().to_string())
            .or_default()
            .push(entry);
    }

    /// Issue a fresh challenge for `service_id`, alongside any already
    /// outstanding.
    ///
    /// This used to replace the outstanding challenge for a service, on the
    /// reasoning that replacing stops a requester piling up valid nonces.
    /// That reasoning was sound about accumulation and wrong about who could
    /// trigger it. Issuing cannot require a signature — demanding one to
    /// obtain the thing you sign is circular — so it is an anonymous write,
    /// and keying by service meant ANY caller could request a challenge for
    /// somebody else's `service_id` in a loop and invalidate the one that
    /// provider was part-way through signing. That is an unauthenticated
    /// denial of service against `getProviderEarnings`, needing no stake, no
    /// credentials and no relationship to the service.
    ///
    /// The shared ledger keys by nonce instead, so an outstanding challenge
    /// is reachable only by someone who already knows its random 32 bytes.
    /// Accumulation is still handled, just not by clobbering: it prunes
    /// expired entries on issue and caps outstanding at
    /// [`ChallengeLedger::MAX_OUTSTANDING`].
    pub fn issue_challenge(&mut self, service_id: &ServiceId, now: Timestamp) -> EarningsChallenge {
        let challenge = self
            .challenges
            .issue(service_id.as_str(), now, CHALLENGE_TTL_SECS);
        EarningsChallenge::from_challenge(challenge, service_id)
    }

    /// Consume the outstanding challenge for `service_id` if `nonce`
    /// matches and it has not expired.
    ///
    /// Consumption happens whether or not the caller's signature later
    /// verifies, which is what makes a captured signature useless: the
    /// nonce it authorises is spent on first presentation.
    pub fn consume_challenge(
        &mut self,
        service_id: &ServiceId,
        nonce: &str,
        now: Timestamp,
    ) -> Result<EarningsChallenge, RegistryError> {
        // The shared ledger removes before checking the subject, so a nonce
        // presented against the wrong service is spent rather than left
        // available to try again elsewhere.
        self.challenges
            .consume(service_id.as_str(), nonce, now)
            .map(|challenge| EarningsChallenge::from_challenge(challenge, service_id))
            .map_err(|error| match error {
                ChallengeError::Unknown => RegistryError::UnknownChallenge,
                ChallengeError::Expired => RegistryError::ChallengeExpired,
            })
    }

    /// How many challenges are outstanding. Diagnostics only — exposed
    /// because accumulation is now the trade-off this design accepts in
    /// exchange for removing the lockout, so it is worth being able to see.
    pub fn outstanding_challenges(&self) -> usize {
        self.challenges.outstanding()
    }

    /// This service's statement. Empty until something credits it.
    pub fn statement(
        &self,
        service_id: &ServiceId,
        payout_wallet: Option<String>,
    ) -> ProviderEarnings {
        ProviderEarnings {
            service_id: service_id.clone(),
            payout_wallet,
            entries: self
                .entries
                .get(service_id.as_str())
                .cloned()
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc() -> ServiceId {
        ServiceId::new("svc-1")
    }

    #[test]
    fn a_fresh_service_has_an_empty_statement() {
        let ledger = EarningsLedger::new();
        let statement = ledger.statement(&svc(), Some("wallet".to_string()));
        assert!(statement.entries.is_empty());
        assert!(statement.totals().is_empty());
    }

    #[test]
    fn credits_accumulate_per_token() {
        let mut ledger = EarningsLedger::new();
        for amount in [10u64, 32] {
            ledger.credit(
                &svc(),
                EarningEntry {
                    token_mint: "MINT-A".to_string(),
                    amount: Amount::new(amount, 6),
                    reference: "trade-1".to_string(),
                    credited_at: Timestamp::from_millis(1),
                },
            );
        }
        ledger.credit(
            &svc(),
            EarningEntry {
                token_mint: "MINT-B".to_string(),
                amount: Amount::new(5, 9),
                reference: "trade-2".to_string(),
                credited_at: Timestamp::from_millis(2),
            },
        );

        let totals = ledger.statement(&svc(), None).totals();
        assert_eq!(totals.len(), 2, "each token totals separately");
        assert_eq!(
            totals
                .iter()
                .find(|(mint, _)| mint == "MINT-A")
                .unwrap()
                .1
                .base_units(),
            42
        );
    }

    #[test]
    fn a_challenge_is_single_use() {
        let mut ledger = EarningsLedger::new();
        let now = Timestamp::from_millis(1_000);
        let challenge = ledger.issue_challenge(&svc(), now);

        assert!(
            ledger
                .consume_challenge(&svc(), &challenge.nonce, now)
                .is_ok()
        );
        assert_eq!(
            ledger.consume_challenge(&svc(), &challenge.nonce, now),
            Err(RegistryError::UnknownChallenge),
            "replaying a spent nonce must fail"
        );
    }

    #[test]
    fn an_expired_challenge_is_rejected() {
        let mut ledger = EarningsLedger::new();
        let issued_at = Timestamp::from_millis(1_000);
        let challenge = ledger.issue_challenge(&svc(), issued_at);
        let too_late =
            Timestamp::from_millis(issued_at.as_millis() + CHALLENGE_TTL_SECS * 1_000 + 1);

        assert_eq!(
            ledger.consume_challenge(&svc(), &challenge.nonce, too_late),
            Err(RegistryError::ChallengeExpired)
        );
    }

    #[test]
    fn a_nonce_issued_for_one_service_does_not_open_another() {
        let mut ledger = EarningsLedger::new();
        let now = Timestamp::from_millis(1_000);
        let mine = ledger.issue_challenge(&svc(), now);
        let other = ServiceId::new("svc-2");
        ledger.issue_challenge(&other, now);

        assert_eq!(
            ledger.consume_challenge(&other, &mine.nonce, now),
            Err(RegistryError::UnknownChallenge)
        );
        // And the bytes signed differ even for an identical nonce, so a
        // signature cannot be lifted between services.
        let a = EarningsChallenge {
            service_id: svc(),
            nonce: "ff".to_string(),
            expires_at: now,
        };
        let b = EarningsChallenge {
            service_id: other,
            nonce: "ff".to_string(),
            expires_at: now,
        };
        assert_ne!(a.signing_bytes(), b.signing_bytes());
    }

    #[test]
    fn two_challenges_never_repeat_a_nonce() {
        let mut ledger = EarningsLedger::new();
        let now = Timestamp::from_millis(1);
        let first = ledger.issue_challenge(&svc(), now).nonce;
        let second = ledger.issue_challenge(&svc(), now).nonce;
        assert_ne!(first, second);
        assert_eq!(first.len(), 64, "32 random bytes, hex-encoded");
    }

    #[test]
    fn reissuing_leaves_the_previous_challenge_usable() {
        // The inverse of what this asserted before. Replacing on reissue was
        // deliberate — it stopped nonces accumulating — but it made issuance,
        // which cannot require a signature, able to destroy somebody else's
        // in-flight handshake.
        let mut ledger = EarningsLedger::new();
        let now = Timestamp::from_millis(1_000);
        let first = ledger.issue_challenge(&svc(), now);
        ledger.issue_challenge(&svc(), now);
        assert!(
            ledger.consume_challenge(&svc(), &first.nonce, now).is_ok(),
            "a second issue must not invalidate a nonce already in flight"
        );
    }

    #[test]
    fn a_stranger_requesting_challenges_cannot_lock_a_service_out() {
        // The vulnerability this migration exists to close, stated as a test
        // so it cannot come back. `issue_challenge` takes no signature and
        // cannot: demanding one to obtain the thing you sign is circular. So
        // anyone can call it for any service_id. Under the previous
        // subject-keyed storage each such call destroyed the provider's
        // outstanding challenge, denying it `getProviderEarnings` for as long
        // as the caller kept going — no stake, no credentials, no
        // relationship to the service required.
        let mut ledger = EarningsLedger::new();
        let now = Timestamp::from_millis(1_000);
        let genuine = ledger.issue_challenge(&svc(), now);

        for _ in 0..64 {
            ledger.issue_challenge(&svc(), now);
        }

        assert!(
            ledger
                .consume_challenge(&svc(), &genuine.nonce, now)
                .is_ok(),
            "the provider's own nonce must survive an attacker's flood"
        );
    }

    #[test]
    fn accumulated_challenges_stay_bounded() {
        // The cost of not clobbering: unanswered challenges pile up instead of
        // replacing each other. Bounded by the shared ledger rather than left
        // to grow, so removing the lockout does not open a memory-exhaustion
        // route in its place.
        let mut ledger = EarningsLedger::new();
        let now = Timestamp::from_millis(1_000);
        for _ in 0..ChallengeLedger::MAX_OUTSTANDING + 128 {
            ledger.issue_challenge(&svc(), now);
        }
        assert!(
            ledger.outstanding_challenges() <= ChallengeLedger::MAX_OUTSTANDING,
            "outstanding challenges must stay capped, got {}",
            ledger.outstanding_challenges()
        );
    }
}
