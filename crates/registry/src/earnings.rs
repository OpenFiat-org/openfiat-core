//! What a service has earned, and who is allowed to read it
//! (OFS-4100 §9.5).
//!
//! # Nothing credits this ledger yet
//!
//! This is a working ledger that currently reads empty for every service,
//! and that is the honest state rather than an oversight. §9.5 settles the
//! *mechanism* — a declared price, a registered payout wallet, a
//! signature-authenticated statement — and leaves the billing *trigger*
//! open, because it differs by role: a delivered notification has an
//! identifiable beneficiary, while a published exchange rate is read by
//! anyone and has no natural payer at read time. Until a role's metering
//! exists there is nothing to credit, and inventing a charging point to
//! make this look finished would put a number in front of providers that
//! no payment stands behind.
//!
//! [`EarningsLedger::credit`] is the seam each role's metering will call.
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
use openfiat_types::{Amount, ServiceId, Timestamp};
use rand::rngs::{StdRng, SysRng};
use rand::{RngExt, SeedableRng};
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

/// A single-use, expiring challenge bound to one Service ID.
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
    pub fn signing_bytes(&self) -> Vec<u8> {
        format!(
            "openfiat-earnings:{}:{}",
            self.service_id.as_str(),
            self.nonce
        )
        .into_bytes()
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
    challenges: HashMap<String, EarningsChallenge>,
}

impl EarningsLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Credit a service. Nothing calls this yet — see the module docs.
    pub fn credit(&mut self, service_id: &ServiceId, entry: EarningEntry) {
        self.entries
            .entry(service_id.as_str().to_string())
            .or_default()
            .push(entry);
    }

    /// Issue a fresh challenge for `service_id`, replacing any
    /// outstanding one. Replacing rather than accumulating keeps a
    /// requester from piling up valid nonces.
    pub fn issue_challenge(&mut self, service_id: &ServiceId, now: Timestamp) -> EarningsChallenge {
        // Same OS-entropy pattern `openfiat_crypto::Keypair::generate`
        // uses: `SysRng` is fallible-only, so it seeds an infallible
        // `StdRng` once.
        let mut rng = StdRng::try_from_rng(&mut SysRng).expect("OS entropy source unavailable");
        let nonce: [u8; 32] = rng.random();
        let challenge = EarningsChallenge {
            service_id: service_id.clone(),
            nonce: nonce.iter().map(|b| format!("{b:02x}")).collect(),
            expires_at: Timestamp::from_millis(
                now.as_millis().saturating_add(CHALLENGE_TTL_SECS * 1_000),
            ),
        };
        self.challenges
            .insert(service_id.as_str().to_string(), challenge.clone());
        challenge
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
        let key = service_id.as_str().to_string();
        let challenge = self
            .challenges
            .get(&key)
            .ok_or(RegistryError::UnknownChallenge)?;
        if challenge.nonce != nonce {
            return Err(RegistryError::UnknownChallenge);
        }
        let challenge = self
            .challenges
            .remove(&key)
            .expect("just observed under the same key");
        if challenge.expires_at.as_millis() < now.as_millis() {
            return Err(RegistryError::ChallengeExpired);
        }
        Ok(challenge)
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
    fn reissuing_invalidates_the_previous_challenge() {
        let mut ledger = EarningsLedger::new();
        let now = Timestamp::from_millis(1_000);
        let first = ledger.issue_challenge(&svc(), now);
        ledger.issue_challenge(&svc(), now);
        assert_eq!(
            ledger.consume_challenge(&svc(), &first.nonce, now),
            Err(RegistryError::UnknownChallenge),
            "an outstanding nonce must not survive being replaced"
        );
    }
}
