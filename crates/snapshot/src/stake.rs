//! What it costs to be believed: the OPEN a snapshot provider must have
//! locked on chain before this node will adopt its worldview.
//!
//! # The half of the threat model `crate::trust` does not cover
//!
//! [`crate::trust`] states the problem exactly and then solves one case of
//! it. A node with no checkpoint has no basis for preferring an honest
//! producer's snapshot to a self-consistent fabrication, so its *first*
//! snapshot must come from a pinned anchor. That module then says the
//! stake requirement on registration governs afterwards.
//!
//! It did not. Nothing anywhere checked a snapshot provider's stake, and
//! `openfiat_registry` charges nothing to register: becoming a registered
//! `SnapshotProvider` cost a signature. So for a node *with* a checkpoint
//! — which is every node, five minutes after it starts — the entire gate
//! between it and a fabricated worldview was `is_registered_provider`, and
//! that gate was free to walk through. The anchors were not the first line
//! of defence with stake behind them; they were the only line, and only
//! for one import.
//!
//! # What a stake gate does and does not buy
//!
//! It does not make a fabrication detectable. Nothing here inspects what a
//! snapshot claims; a fully-staked provider can announce a lie and it will
//! verify perfectly, exactly as `crate::trust` describes. What it changes
//! is the price. Announcing a forged worldview stops being free and starts
//! costing a stake that `openfiat-staking`'s `slash` can take, held by a
//! wallet the network can name. That is a deterrent and an attribution,
//! not a proof, and it should be described as the former.
//!
//! # Where the number comes from
//!
//! Both places, and the stricter wins:
//!
//! - the on-chain `StakingConfig.min_stake_by_role[SnapshotProvider]`,
//!   which is what governance actually controls and what the staking
//!   program itself enforces at stake time; and
//! - [`MINIMUM_PROVIDER_STAKE`], a floor pinned in this binary.
//!
//! The floor is not a duplicate of the on-chain minimum, because the two
//! answer different questions. The on-chain minimum is the price of
//! *holding a SnapshotProvider stake account* — a registration fee. This
//! is the price of *being believed by this node*, which is a trust
//! decision, and `crate::trust` already argues at length why a node's
//! trust decisions must not be remotely adjustable. A governance
//! parameter that could be voted to zero would silently disable this gate
//! on every node in the network at once, which is precisely the
//! remote-untrusting that module refuses for the anchors. Taking the
//! maximum keeps governance able to *raise* the bar with a parameter write
//! and no code change — the property `StakingConfig::min_stake_by_role`
//! exists to provide — while leaving it unable to lower this node's own
//! floor. Additive-only, the same shape as `TrustAnchors::with_operator`.
//!
//! # Two honest nodes, and how far apart they can be
//!
//! A stake read is a point observation of a remote chain, so nodes cannot
//! be exactly in step. The divergence is bounded rather than open: an
//! observation stands for [`STAKE_OBSERVATION_TTL`] and is refreshed every
//! [`STAKE_REVALIDATION_INTERVAL`], so two honest `RpcConnected` nodes can
//! disagree about a provider for at most one TTL after its stake actually
//! changes. That is the error bar on this gate, stated the way
//! `openfiat_reservations::protocol::SWEEP_INTERVAL` states its own, and
//! it is why a verdict expires instead of being cached for the life of the
//! process: a check made once at registration and never again is a
//! different and much weaker guarantee than one that has to keep being
//! true.
//!
//! # A `GossipOnly` node cannot enforce this, and does not pretend to
//!
//! It has no Solana RPC endpoint, so it cannot read a `StakeAccount` at
//! all — the same wall `openfiat_rpc::actor::discard_unverifiable_votes`
//! hits with governance vote weights. There is no gossip substitute worth
//! having: a peer's assertion about someone else's stake is exactly the
//! self-reported weight the governance fix (#107) removed.
//!
//! So such a node reports [`StakeStanding::Unenforceable`] and falls back
//! to the one credential it *can* evaluate locally — the pinned anchors,
//! applied to every import rather than only the first. It is a real
//! reduction in what a `GossipOnly` node will import from, and it is
//! deliberate: the alternative is a default-mode node that adopts whole
//! worldviews from anyone who can afford a signature. Set
//! `--solana-rpc-url` to enforce the stake gate and import from any
//! qualifying provider.

use crate::error::SnapshotError;
use openfiat_types::{PeerId, PublicKey, Timestamp};
use std::collections::HashMap;
use std::time::Duration;

/// OPEN's base units per whole token (OFS-4100 §1) — nine decimals, the
/// mint's own precision. Quoted so the figure below reads as the number a
/// human would say.
const OPEN: u64 = 1_000_000_000;

/// The stake this node requires of a snapshot provider before it will
/// import from it, floor.
///
/// Ten thousand OPEN, against a deployed on-chain SnapshotProvider
/// minimum of one thousand. The gap is the point and is not an oversight:
/// see this module's own note on why the two numbers answer different
/// questions. A provider clears this by holding ten thousand whatever
/// governance sets the registration minimum to; governance raising its
/// minimum above ten thousand raises this too, with no code change.
pub const MINIMUM_PROVIDER_STAKE: u64 = 10_000 * OPEN;

/// How long a stake observation stands before this node stops treating it
/// as current.
///
/// The divergence window described in this module's own note: after a
/// provider unstakes or is slashed, nodes disagree about whether it still
/// qualifies for at most this long. Ten minutes rather than an hour
/// because the thing being decided is whether to replace a node's entire
/// state, and rather than one minute because a stake does not move on that
/// timescale — `request_unstake` locks a snapshot provider's tokens for
/// seven days afterwards — so a tighter window would buy accuracy nobody
/// can observe while demoting honest providers on every brief RPC outage.
pub const STAKE_OBSERVATION_TTL: Duration = Duration::from_secs(10 * 60);

/// How often an `RpcConnected` node re-reads every registered snapshot
/// provider's stake.
///
/// Declared beside the TTL it refreshes, not beside the timer that drives
/// it, for the reason `openfiat_reservations::protocol::SWEEP_INTERVAL`
/// gives: the two numbers are only meaningful as a pair. Five
/// revalidations fit inside one TTL, clearing the ratio-of-five bar
/// `REGISTRY_SWEEP_INTERVAL` is held to — so a provider's standing lapses
/// only after five consecutive failed reads, and a single unlucky poll
/// never demotes anyone.
pub const STAKE_REVALIDATION_INTERVAL: Duration = Duration::from_secs(2 * 60);

/// The deployed `openfiat-staking` program.
///
/// A second copy of `openfiat_chain::PROGRAM_IDS.staking`, kept here so
/// this crate needs no dependency on the chain bridge — exactly the
/// arrangement, for the same reason, as
/// `openfiat_governance::onchain::GOVERNANCE_PROGRAM_ID`. The test below
/// re-reads the deployment record, so the copy cannot drift into pointing
/// at a program that never wrote a stake account.
pub const STAKING_PROGRAM_ID: &str = "HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx";

/// `sha256("account:StakeAccount")[..8]`, taken verbatim from a real
/// `anchor build`'s `programs/target/idl/staking.json`.
const STAKE_ACCOUNT_DISCRIMINATOR: [u8; 8] = [80, 158, 67, 124, 50, 189, 192, 255];

/// `sha256("account:StakingConfig")[..8]`, likewise.
const STAKING_CONFIG_DISCRIMINATOR: [u8; 8] = [45, 134, 252, 82, 37, 57, 84, 25];

/// `openfiat_programs_shared::Role::SnapshotProvider`'s Borsh
/// discriminant, which is also its `Role::index` into every per-role
/// parameter array.
///
/// Both uses are the same byte by construction — the program derives a
/// `StakeAccount`'s PDA from `role as u8` and indexes `min_stake_by_role`
/// by `Role::index` — so one constant is honest here rather than two that
/// could drift apart while agreeing with nothing.
const SNAPSHOT_PROVIDER_ROLE: u8 = 6;

/// PDA seeds, matching the staking program's own `constants.rs`.
const STAKE_ACCOUNT_SEED: &[u8] = b"stake";
const STAKING_CONFIG_SEED: &[u8] = b"staking_config";

/// Byte offsets into a `StakeAccount`, in the program's declaration
/// order: discriminator(8), owner(32), role(1), amount(8), ...
mod stake_offsets {
    pub const OWNER: usize = 8;
    pub const ROLE: usize = OWNER + 32;
    pub const AMOUNT: usize = ROLE + 1;
    pub const MIN_LEN: usize = AMOUNT + 8;
}

/// Byte offsets into a `StakingConfig`: discriminator(8), admin(32),
/// mint(32), min_stake_by_role([u64; 7]), ...
mod config_offsets {
    pub const MIN_STAKE_BY_ROLE: usize = 8 + 32 + 32;
    pub const MIN_LEN: usize = MIN_STAKE_BY_ROLE + 8 * 7;
}

/// The address of the singleton `StakingConfig`, `[b"staking_config"]`
/// under the staking program.
pub fn staking_config_address() -> String {
    let program = solana_pubkey::Pubkey::from_str_const(STAKING_PROGRAM_ID);
    solana_pubkey::Pubkey::find_program_address(&[STAKING_CONFIG_SEED], &program)
        .0
        .to_string()
}

/// The address of `provider`'s SnapshotProvider stake account,
/// `[b"stake", owner, role]` under the staking program.
///
/// Derived rather than declared, and that is the substantive difference
/// between this and the governance vote path it otherwise follows. A
/// `VoteCast` *names* the stake account it wants counted, so
/// `openfiat_rpc::actor::poll_vote_verifications` has to check afterwards
/// that the account it fetched really belongs to the voter who signed. An
/// address derived from the provider's own announcing key cannot belong to
/// anybody else, so that check is structural here instead of remembered.
///
/// The provider's Ed25519 gossip key *is* the Solana wallet, the same
/// identification `SignedVoteCast::voter_public_key` relies on: a 32-byte
/// Ed25519 public key is a Solana address.
pub fn provider_stake_address(provider: &PublicKey) -> String {
    let program = solana_pubkey::Pubkey::from_str_const(STAKING_PROGRAM_ID);
    solana_pubkey::Pubkey::find_program_address(
        &[
            STAKE_ACCOUNT_SEED,
            provider.as_bytes(),
            &[SNAPSHOT_PROVIDER_ROLE],
        ],
        &program,
    )
    .0
    .to_string()
}

/// The staked (not unbonding) OPEN held by a snapshot provider, from the
/// raw bytes of the account [`provider_stake_address`] names.
///
/// `owner` is the account's owning program as the RPC reported it, and is
/// checked rather than assumed. Without that, a node would believe
/// whatever balance it found at an address anyone is free to fund — an
/// attacker's own staking program writes a convincing `StakeAccount` for
/// the price of a deployment.
///
/// The role byte is checked too. The PDA derivation already pins it, so
/// this cannot fail against a genuinely-derived address; it is here so
/// that the one thing this function returns — a number that decides
/// whether a worldview is adopted — is never read out of an account whose
/// own layout says it is something else.
pub fn decode_provider_stake(owner: &str, data: &[u8]) -> Result<u64, SnapshotError> {
    if owner != STAKING_PROGRAM_ID {
        return Err(SnapshotError::ForeignStakeAccount);
    }
    if data.len() < stake_offsets::MIN_LEN || data[..8] != STAKE_ACCOUNT_DISCRIMINATOR {
        return Err(SnapshotError::MalformedStakeAccount);
    }
    if data[stake_offsets::ROLE] != SNAPSHOT_PROVIDER_ROLE {
        return Err(SnapshotError::MalformedStakeAccount);
    }
    Ok(u64::from_le_bytes(
        data[stake_offsets::AMOUNT..stake_offsets::AMOUNT + 8]
            .try_into()
            .expect("slice is exactly 8 bytes"),
    ))
}

/// Governance's own minimum for the SnapshotProvider role, from the raw
/// bytes of the account [`staking_config_address`] names.
///
/// Returned as the program states it. Combining it with this node's floor
/// is [`ProviderStakes::observe_requirement`]'s job, kept separate so that
/// what the chain says and what this node requires never become one
/// unlabelled number.
pub fn decode_required_stake(owner: &str, data: &[u8]) -> Result<u64, SnapshotError> {
    if owner != STAKING_PROGRAM_ID {
        return Err(SnapshotError::ForeignStakeAccount);
    }
    if data.len() < config_offsets::MIN_LEN || data[..8] != STAKING_CONFIG_DISCRIMINATOR {
        return Err(SnapshotError::MalformedStakeAccount);
    }
    let at = config_offsets::MIN_STAKE_BY_ROLE + 8 * SNAPSHOT_PROVIDER_ROLE as usize;
    Ok(u64::from_le_bytes(
        data[at..at + 8]
            .try_into()
            .expect("slice is exactly 8 bytes"),
    ))
}

/// Where a provider stands with this node, right now.
///
/// Four outcomes rather than a boolean, because they are fixed by
/// different people and collapsing them loses the only thing an operator
/// staring at a refusal needs to know: whether the provider must stake
/// more, or this node must be given an RPC endpoint, or nobody need do
/// anything because the next poll will settle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StakeStanding {
    /// Read from chain within [`STAKE_OBSERVATION_TTL`], and sufficient.
    Qualified,
    /// Read from chain within [`STAKE_OBSERVATION_TTL`], and short.
    Insufficient { held: u64, required: u64 },
    /// This node enforces the gate but has no current reading for this
    /// provider — never polled, or every reading has aged out. Not a
    /// judgement about the provider; an absence of one.
    Unread,
    /// This node is `GossipOnly` and can never answer the question. See
    /// this module's own note.
    Unenforceable,
}

#[derive(Debug, Clone, Copy)]
struct Observation {
    amount: u64,
    at: Timestamp,
}

/// This node's standing record of who holds a qualifying snapshot-provider
/// stake.
///
/// Shared (`Rc<RefCell<_>>`) between the poll loop that writes it and the
/// [`crate::SnapshotIndex`] that reads it, so the index consults one
/// answer rather than caching its own. A second copy is how "checked at
/// announce" and "checked at import" come to disagree.
#[derive(Debug)]
pub struct ProviderStakes {
    enforced: bool,
    required: u64,
    seen: HashMap<PeerId, Observation>,
}

impl ProviderStakes {
    /// A node that can read the chain and therefore enforces the gate.
    pub fn enforcing() -> Self {
        Self {
            enforced: true,
            required: MINIMUM_PROVIDER_STAKE,
            seen: HashMap::new(),
        }
    }

    /// A node that cannot read the chain — every standing is
    /// [`StakeStanding::Unenforceable`], and the anchors govern instead.
    ///
    /// This is [`Default`] deliberately: a node that has not been told it
    /// can read the chain has not been told it can, and the failure that
    /// mode produces is a refusal to import rather than an import that
    /// was never checked.
    pub fn unenforceable() -> Self {
        Self {
            enforced: false,
            required: MINIMUM_PROVIDER_STAKE,
            seen: HashMap::new(),
        }
    }

    pub fn is_enforced(&self) -> bool {
        self.enforced
    }

    /// What this node currently demands, in OPEN base units.
    pub fn required(&self) -> u64 {
        self.required
    }

    /// Adopt governance's minimum, never below this node's own floor.
    ///
    /// See this module's note on why the maximum rather than the chain's
    /// figure outright.
    pub fn observe_requirement(&mut self, onchain_minimum: u64) {
        self.required = onchain_minimum.max(MINIMUM_PROVIDER_STAKE);
    }

    /// Record a stake read from chain at `at`.
    pub fn observe(&mut self, provider: PeerId, amount: u64, at: Timestamp) {
        self.seen.insert(provider, Observation { amount, at });
    }

    /// Drop the provider's reading outright — for a provider whose stake
    /// account has been *positively* observed not to exist, which is a
    /// different fact from a read that failed and must not be recorded as
    /// a zero balance that then ages out like a real one.
    pub fn forget(&mut self, provider: &PeerId) {
        self.seen.remove(provider);
    }

    /// Where `provider` stands at `now`.
    pub fn standing(&self, provider: &PeerId, now: Timestamp) -> StakeStanding {
        if !self.enforced {
            return StakeStanding::Unenforceable;
        }
        let Some(observation) = self.seen.get(provider) else {
            return StakeStanding::Unread;
        };
        // Saturating, so a clock that moved backwards between the
        // observation and this call reports an age of zero rather than
        // wrapping to an enormous one and expiring every reading at once.
        let age = now.as_millis().saturating_sub(observation.at.as_millis());
        if age > STAKE_OBSERVATION_TTL.as_millis() as u64 {
            return StakeStanding::Unread;
        }
        if observation.amount >= self.required {
            StakeStanding::Qualified
        } else {
            StakeStanding::Insufficient {
                held: observation.amount,
                required: self.required,
            }
        }
    }

    /// How many current readings this node holds — for diagnostics, and
    /// for a test that wants to assert an aged-out reading is genuinely
    /// gone from the answer rather than merely unreported.
    pub fn current_count(&self, now: Timestamp) -> usize {
        self.seen
            .keys()
            .filter(|provider| self.standing(provider, now) != StakeStanding::Unread)
            .count()
    }
}

impl Default for ProviderStakes {
    fn default() -> Self {
        Self::unenforceable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_crypto::Keypair;
    use openfiat_network::identity::peer_id_from_public_key;

    fn peer(seed: u8) -> PeerId {
        peer_id_from_public_key(&Keypair::from_seed([seed; 32]).public_key()).unwrap()
    }

    fn stake_account_bytes(role: u8, amount: u64) -> Vec<u8> {
        let mut bytes = vec![0u8; stake_offsets::MIN_LEN];
        bytes[..8].copy_from_slice(&STAKE_ACCOUNT_DISCRIMINATOR);
        bytes[stake_offsets::ROLE] = role;
        bytes[stake_offsets::AMOUNT..stake_offsets::AMOUNT + 8]
            .copy_from_slice(&amount.to_le_bytes());
        bytes
    }

    fn staking_config_bytes(min_stake_by_role: [u64; 7]) -> Vec<u8> {
        let mut bytes = vec![0u8; config_offsets::MIN_LEN];
        bytes[..8].copy_from_slice(&STAKING_CONFIG_DISCRIMINATOR);
        for (index, minimum) in min_stake_by_role.iter().enumerate() {
            let at = config_offsets::MIN_STAKE_BY_ROLE + index * 8;
            bytes[at..at + 8].copy_from_slice(&minimum.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn decodes_a_snapshot_providers_staked_amount() {
        let decoded =
            decode_provider_stake(STAKING_PROGRAM_ID, &stake_account_bytes(6, 12_345)).unwrap();
        assert_eq!(decoded, 12_345);
    }

    #[test]
    fn an_account_owned_by_another_program_is_refused() {
        // The whole soundness of this gate rests on it. Anyone may create
        // an account at an address of their choosing, or deploy their own
        // staking program, and fill it with a qualifying balance.
        assert_eq!(
            decode_provider_stake(
                "AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE",
                &stake_account_bytes(6, MINIMUM_PROVIDER_STAKE)
            ),
            Err(SnapshotError::ForeignStakeAccount)
        );
    }

    #[test]
    fn an_account_with_another_types_discriminator_is_refused() {
        let mut bytes = stake_account_bytes(6, 100);
        bytes[0] ^= 0xFF;
        assert_eq!(
            decode_provider_stake(STAKING_PROGRAM_ID, &bytes),
            Err(SnapshotError::MalformedStakeAccount)
        );
    }

    #[test]
    fn a_stake_held_under_another_role_does_not_count_as_a_snapshot_providers() {
        // A merchant's 500-OPEN stake is a real, program-written
        // StakeAccount. It is not a snapshot provider's bond, and the
        // role byte is the only thing that says so.
        assert_eq!(
            decode_provider_stake(
                STAKING_PROGRAM_ID,
                &stake_account_bytes(0, MINIMUM_PROVIDER_STAKE)
            ),
            Err(SnapshotError::MalformedStakeAccount)
        );
    }

    #[test]
    fn truncated_account_data_is_refused_rather_than_read_past() {
        assert_eq!(
            decode_provider_stake(STAKING_PROGRAM_ID, &STAKE_ACCOUNT_DISCRIMINATOR),
            Err(SnapshotError::MalformedStakeAccount)
        );
    }

    #[test]
    fn reads_the_snapshot_provider_slot_of_the_governance_minimum_and_no_other() {
        // Every role gets a distinct value, so an off-by-one in the
        // offset arithmetic shows up as the wrong role's number rather
        // than as a coincidence.
        let decoded = decode_required_stake(
            STAKING_PROGRAM_ID,
            &staking_config_bytes([500, 501, 1_000, 5_000, 1_001, 1_002, 7_777]),
        )
        .unwrap();
        assert_eq!(decoded, 7_777);
    }

    #[test]
    fn a_staking_config_from_another_program_is_refused() {
        assert_eq!(
            decode_required_stake(
                "11111111111111111111111111111111",
                &staking_config_bytes([0; 7])
            ),
            Err(SnapshotError::ForeignStakeAccount)
        );
    }

    #[test]
    fn governance_may_raise_the_requirement_but_not_lower_this_nodes_floor() {
        // The additive-only property, and the whole argument for pinning
        // a floor at all: a parameter voted to zero must not disable this
        // gate on every node at once.
        let mut stakes = ProviderStakes::enforcing();
        stakes.observe_requirement(0);
        assert_eq!(stakes.required(), MINIMUM_PROVIDER_STAKE);
        stakes.observe_requirement(1_000 * OPEN);
        assert_eq!(stakes.required(), MINIMUM_PROVIDER_STAKE);
        stakes.observe_requirement(50_000 * OPEN);
        assert_eq!(stakes.required(), 50_000 * OPEN);
    }

    #[test]
    fn a_provider_holding_the_requirement_qualifies_and_one_below_it_does_not() {
        let now = Timestamp::from_millis(1_000_000);
        let mut stakes = ProviderStakes::enforcing();
        stakes.observe(peer(1), MINIMUM_PROVIDER_STAKE, now);
        stakes.observe(peer(2), MINIMUM_PROVIDER_STAKE - 1, now);

        assert_eq!(stakes.standing(&peer(1), now), StakeStanding::Qualified);
        assert_eq!(
            stakes.standing(&peer(2), now),
            StakeStanding::Insufficient {
                held: MINIMUM_PROVIDER_STAKE - 1,
                required: MINIMUM_PROVIDER_STAKE,
            }
        );
    }

    #[test]
    fn a_provider_nobody_has_read_is_unread_rather_than_insufficient() {
        // "We have not looked" and "they are short" are fixed by
        // different people, and a node that reported the second when it
        // meant the first would send an honest operator to top up a stake
        // that was already fine.
        let now = Timestamp::from_millis(1_000_000);
        assert_eq!(
            ProviderStakes::enforcing().standing(&peer(1), now),
            StakeStanding::Unread
        );
    }

    #[test]
    fn a_reading_older_than_the_ttl_stops_counting() {
        // The property that makes this a standing requirement rather than
        // a one-off check: a provider verified once does not stay
        // verified for the life of the process.
        let observed = Timestamp::from_millis(1_000_000);
        let mut stakes = ProviderStakes::enforcing();
        stakes.observe(peer(1), MINIMUM_PROVIDER_STAKE, observed);

        let ttl = STAKE_OBSERVATION_TTL.as_millis() as u64;
        let still_fresh = Timestamp::from_millis(observed.as_millis() + ttl);
        assert_eq!(
            stakes.standing(&peer(1), still_fresh),
            StakeStanding::Qualified
        );
        assert_eq!(stakes.current_count(still_fresh), 1);

        let expired = Timestamp::from_millis(observed.as_millis() + ttl + 1);
        assert_eq!(stakes.standing(&peer(1), expired), StakeStanding::Unread);
        assert_eq!(stakes.current_count(expired), 0);
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_expire_every_reading_at_once() {
        let observed = Timestamp::from_millis(1_000_000);
        let mut stakes = ProviderStakes::enforcing();
        stakes.observe(peer(1), MINIMUM_PROVIDER_STAKE, observed);
        assert_eq!(
            stakes.standing(&peer(1), Timestamp::from_millis(1)),
            StakeStanding::Qualified
        );
    }

    #[test]
    fn a_gossip_only_node_says_it_cannot_enforce_rather_than_answering() {
        // Not `Unread`, which would imply a later poll will settle it,
        // and emphatically not `Qualified`. See this module's own note.
        let now = Timestamp::from_millis(1_000_000);
        let mut stakes = ProviderStakes::unenforceable();
        stakes.observe(peer(1), MINIMUM_PROVIDER_STAKE * 10, now);
        assert_eq!(
            stakes.standing(&peer(1), now),
            StakeStanding::Unenforceable,
            "a node with no RPC endpoint cannot have read anyone's stake"
        );
        assert_eq!(
            ProviderStakes::default().standing(&peer(1), now),
            StakeStanding::Unenforceable,
            "and the default must be the mode that cannot be wrong by accident"
        );
    }

    #[test]
    fn a_provider_that_unstaked_is_disbelieved_at_the_next_reading() {
        let now = Timestamp::from_millis(1_000_000);
        let mut stakes = ProviderStakes::enforcing();
        stakes.observe(peer(1), MINIMUM_PROVIDER_STAKE, now);
        assert_eq!(stakes.standing(&peer(1), now), StakeStanding::Qualified);

        stakes.observe(peer(1), 0, now);
        assert_eq!(
            stakes.standing(&peer(1), now),
            StakeStanding::Insufficient {
                held: 0,
                required: MINIMUM_PROVIDER_STAKE,
            }
        );

        stakes.forget(&peer(1));
        assert_eq!(stakes.standing(&peer(1), now), StakeStanding::Unread);
    }

    /// The program id this crate is willing to believe, checked against
    /// the record of what was actually deployed. A typo would leave every
    /// stake read rejected as foreign, which looks exactly like a network
    /// where nobody has staked.
    #[test]
    fn the_pinned_program_id_matches_the_deployment_record() {
        const ADDRESSES: &str = include_str!("../../../programs/devnet-addresses.json");
        let addresses: serde_json::Value =
            serde_json::from_str(ADDRESSES).expect("devnet-addresses.json must be valid");
        assert_eq!(
            addresses["devnet_programs"]["staking"].as_str(),
            Some(STAKING_PROGRAM_ID)
        );
    }

    /// The layouts this module hard-codes, checked against the IDL a real
    /// `anchor build` produced.
    ///
    /// Without this, a field added to `StakeAccount` or `StakingConfig`
    /// shifts every offset here and the decoders keep returning confident
    /// nonsense — reading, say, `unbonding_amount` as the staked balance,
    /// or the arbitrator's minimum as the snapshot provider's. Read at run
    /// time rather than with `include_str!` because `programs/target` is
    /// generated and git-ignored, and reported as skipped when absent
    /// rather than passing quietly: a test that claims success when it
    /// could not look is worse than no test. This mirrors
    /// `openfiat_governance::onchain`'s check exactly.
    #[test]
    fn the_hard_coded_layouts_still_match_the_programs_own_idl() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../programs/target/idl/staking.json"
        );
        let Ok(raw) = std::fs::read_to_string(path) else {
            eprintln!(
                "SKIPPED: {path} is absent, so this crate's hard-coded StakeAccount and \
                 StakingConfig layouts could not be checked against the program's own IDL. Run \
                 `anchor build` in openfiat-core/programs to enable it."
            );
            return;
        };
        let idl: serde_json::Value =
            serde_json::from_str(&raw).expect("the IDL must be valid JSON");

        let discriminator = |name: &str| -> Vec<u8> {
            idl["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|account| account["name"] == name)
                .unwrap_or_else(|| panic!("the program must still declare a {name} account"))
                ["discriminator"]
                .as_array()
                .unwrap()
                .iter()
                .map(|byte| byte.as_u64().unwrap() as u8)
                .collect()
        };
        assert_eq!(discriminator("StakeAccount"), STAKE_ACCOUNT_DISCRIMINATOR);
        assert_eq!(discriminator("StakingConfig"), STAKING_CONFIG_DISCRIMINATOR);

        // Widths come from the IDL's own type names rather than being
        // assumed, so a field whose *type* changed is caught as well as
        // one that was inserted.
        fn width(ty: &serde_json::Value) -> usize {
            if let Some(name) = ty.as_str() {
                return match name {
                    "u8" | "bool" => 1,
                    "u16" => 2,
                    "i64" | "u64" => 8,
                    "pubkey" => 32,
                    other => panic!("unhandled IDL scalar type {other}"),
                };
            }
            if let Some(array) = ty.get("array") {
                return width(&array[0]) * array[1].as_u64().unwrap() as usize;
            }
            // `Role` is the only `defined` type on either account, and is
            // a fieldless enum: one Borsh byte.
            assert!(ty.get("defined").is_some(), "unhandled IDL type {ty}");
            1
        }

        let offsets_of = |name: &str| -> std::collections::HashMap<String, usize> {
            let fields = idl["types"]
                .as_array()
                .unwrap()
                .iter()
                .find(|ty| ty["name"] == name)
                .unwrap_or_else(|| panic!("the IDL must describe {name}'s fields"))["type"]
                ["fields"]
                .as_array()
                .unwrap()
                .clone();
            let mut offset = 8; // the discriminator
            let mut offsets = std::collections::HashMap::new();
            for field in &fields {
                offsets.insert(field["name"].as_str().unwrap().to_string(), offset);
                offset += width(&field["type"]);
            }
            offsets
        };

        let stake = offsets_of("StakeAccount");
        assert_eq!(stake.get("owner"), Some(&stake_offsets::OWNER));
        assert_eq!(stake.get("role"), Some(&stake_offsets::ROLE));
        assert_eq!(stake.get("amount"), Some(&stake_offsets::AMOUNT));

        let config = offsets_of("StakingConfig");
        assert_eq!(
            config.get("min_stake_by_role"),
            Some(&config_offsets::MIN_STAKE_BY_ROLE)
        );
    }

    /// Pinned against addresses derived by an independent implementation
    /// of `find_program_address` — same construction the staking
    /// program's own PDAs and `@solana/web3.js`'s `findProgramAddressSync`
    /// use, run outside this crate rather than by calling the code under
    /// test.
    ///
    /// The literals are what make this worth having: an address derived
    /// with the wrong seed, the wrong role byte or the wrong program
    /// would read an empty account for every provider, which looks
    /// exactly like a network where nobody has staked — and a gate that
    /// nobody can pass fails as silently as one nobody can fail.
    #[test]
    fn the_derived_addresses_match_what_a_solana_client_derives() {
        assert_eq!(
            staking_config_address(),
            "2wrGGjcUFSn1ZiYzo2o64r7ZC88QNhvvgUYktNs2ifT9"
        );
        let provider = Keypair::from_seed([7u8; 32]).public_key();
        assert_eq!(
            provider_stake_address(&provider),
            "G73zLXLtW19EY5BSb28XNt5ubWah4raYo53dM56d6vqQ"
        );
    }
}
