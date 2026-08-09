//! Keeps this node's record of who is actually staked as a snapshot
//! provider current, by reading the staking program directly.
//!
//! `openfiat_snapshot::stake` decides what the requirement is and what a
//! given reading means; this module is only the part that has to talk to a
//! Solana RPC endpoint, and lives here for the same reason
//! `actor::poll_vote_verifications` does — `openfiat-snapshot` deliberately
//! depends on no chain client, and a crate that derives and decodes should
//! not also fetch.
//!
//! Structurally this is the governance vote-weight verification (#107)
//! again, with one deliberate difference. A vote *names* the stake account
//! it wants counted, so that path has to fetch a caller-supplied address
//! and check afterwards that it belongs to the voter. Here the address is
//! derived from the provider's own announcing key
//! (`openfiat_snapshot::stake::provider_stake_address`), so it cannot
//! belong to anybody else and there is no claim to check.
//!
//! The second difference is the shape of the work. Vote verification is
//! fed by gossip and therefore needs a bounded retry queue; this walks the
//! service registry, which is already bounded and already swept, so there
//! is no queue to grow and nothing to give up on. Every provider is
//! re-read every tick, which is what makes a stake a standing requirement
//! rather than a one-off check — see
//! `openfiat_snapshot::stake::STAKE_OBSERVATION_TTL` for the divergence
//! window that follows from it.

use crate::state::NodeState;
use openfiat_chain::ChainClient;
use openfiat_snapshot::stake;
use openfiat_storage::KvStore;
use openfiat_types::{InfrastructureService, ServiceType, Timestamp};

/// Re-reads governance's minimum and every registered snapshot provider's
/// stake account.
///
/// Every failure leaves the previous reading in place to age out on its
/// own rather than demoting the provider immediately: an RPC endpoint
/// being briefly unreachable is not evidence that anybody unstaked, and
/// `STAKE_OBSERVATION_TTL` already bounds how long a stale reading may
/// stand. What *is* treated as evidence is the endpoint answering that the
/// account does not exist, which is a fact about the provider and not
/// about the connection.
pub async fn poll_provider_stake<S: KvStore + 'static>(
    state: &NodeState<S>,
    client: &dyn ChainClient,
) {
    // A `GossipOnly` node never reaches here (the actor only polls with a
    // client), but the guard is cheap and states the invariant where it
    // can be checked rather than leaving it to the call site: recording
    // observations into an unenforceable register would produce a node
    // that has readings and still refuses to use them.
    if !state.provider_stakes.borrow().is_enforced() {
        return;
    }

    // No config account, or an endpoint that could not be reached, leaves
    // the pinned floor standing — which is what this node requires either
    // way, so there is nothing to fall back to and nothing to report.
    let config_address = stake::staking_config_address();
    if let Ok(Some((owner, data))) = client.get_account(&config_address).await {
        match stake::decode_required_stake(&owner, &data) {
            Ok(minimum) => state
                .provider_stakes
                .borrow_mut()
                .observe_requirement(minimum),
            Err(error) => eprintln!(
                "openfiat-rpc: ignoring the on-chain staking config at {config_address} — {error}; \
                 this node's own snapshot-provider floor stands"
            ),
        }
    }

    for service in state.services.all() {
        if !matches!(
            service.service_type,
            ServiceType::Infrastructure(InfrastructureService::SnapshotProvider)
        ) {
            continue;
        }
        let address = stake::provider_stake_address(&service.provider_public_key);
        match client.get_account(&address).await {
            Ok(Some((owner, data))) => match stake::decode_provider_stake(&owner, &data) {
                Ok(amount) => {
                    state.provider_stakes.borrow_mut().observe(
                        service.provider.clone(),
                        amount,
                        Timestamp::now(),
                    );
                }
                Err(error) => {
                    // Something is at the derived address and it is not a
                    // snapshot provider's stake account. Said out loud
                    // rather than silently treated as zero: on a correct
                    // deployment this cannot happen, so it means the
                    // pinned program id or the account layout has moved.
                    eprintln!(
                        "openfiat-rpc: refusing to read {address} as {}'s snapshot-provider stake \
                         — {error}",
                        service.provider
                    );
                    state.provider_stakes.borrow_mut().forget(&service.provider);
                }
            },
            // The cluster says there is no such account: this provider has
            // never staked under the SnapshotProvider role. A fact, and
            // recorded as one — it is what lets `apply_announce` stop
            // relaying an unbacked announcement.
            Ok(None) => {
                state.provider_stakes.borrow_mut().observe(
                    service.provider.clone(),
                    0,
                    Timestamp::now(),
                );
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openfiat_chain::{ChainError, NodeChainMode, SignatureStatus};
    use openfiat_crypto::Keypair;
    use openfiat_network::Node;
    use openfiat_network::identity::peer_id_from_public_key;
    use openfiat_registry::{Registration, SignedRegistration};
    use openfiat_snapshot::StakeStanding;
    use openfiat_snapshot::stake::MINIMUM_PROVIDER_STAKE;
    use openfiat_storage::mem::MemoryStore;
    use openfiat_types::{PublicKey, ServiceId};
    use std::collections::HashMap;

    /// Answers `get_account` from a fixed map of address to
    /// `(owner_program, data)`, so a test names the *addresses* it expects
    /// this module to derive. A poll that derived the wrong PDA gets
    /// `None` here, which is exactly what a real cluster would say.
    struct FixtureCluster {
        accounts: HashMap<String, (String, Vec<u8>)>,
        /// Every lookup fails, standing in for an unreachable endpoint.
        offline: bool,
    }

    impl FixtureCluster {
        fn new() -> Self {
            Self {
                accounts: HashMap::new(),
                offline: false,
            }
        }

        fn offline() -> Self {
            Self {
                accounts: HashMap::new(),
                offline: true,
            }
        }

        fn with_config(mut self, min_stake_by_role: [u64; 7]) -> Self {
            let mut data = vec![0u8; 8 + 32 + 32 + 8 * 7];
            data[..8].copy_from_slice(&[45, 134, 252, 82, 37, 57, 84, 25]);
            for (index, minimum) in min_stake_by_role.iter().enumerate() {
                let at = 8 + 32 + 32 + index * 8;
                data[at..at + 8].copy_from_slice(&minimum.to_le_bytes());
            }
            self.accounts.insert(
                stake::staking_config_address(),
                (stake::STAKING_PROGRAM_ID.to_string(), data),
            );
            self
        }

        fn with_stake(mut self, provider: &PublicKey, role: u8, amount: u64) -> Self {
            let mut data = vec![0u8; 8 + 32 + 1 + 8];
            data[..8].copy_from_slice(&[80, 158, 67, 124, 50, 189, 192, 255]);
            data[8..40].copy_from_slice(provider.as_bytes());
            data[40] = role;
            data[41..49].copy_from_slice(&amount.to_le_bytes());
            self.accounts.insert(
                stake::provider_stake_address(provider),
                (stake::STAKING_PROGRAM_ID.to_string(), data),
            );
            self
        }
    }

    #[async_trait::async_trait]
    impl ChainClient for FixtureCluster {
        async fn get_latest_blockhash(&self) -> Result<(String, u64), ChainError> {
            Ok(("unused".to_string(), 0))
        }
        async fn is_blockhash_valid(&self, _blockhash: &str) -> Result<bool, ChainError> {
            Ok(true)
        }
        async fn send_transaction(&self, _tx_bytes: &[u8]) -> Result<String, ChainError> {
            Ok("unused".to_string())
        }
        async fn get_signature_status(
            &self,
            _signature: &str,
        ) -> Result<Option<SignatureStatus>, ChainError> {
            Ok(None)
        }
        async fn get_account(&self, pubkey: &str) -> Result<Option<(String, Vec<u8>)>, ChainError> {
            if self.offline {
                return Err(ChainError::ChainUnavailable);
            }
            Ok(self.accounts.get(pubkey).cloned())
        }
    }

    /// An `RpcConnected` node, which is the only mode that enforces this.
    fn rpc_connected_node() -> NodeState<MemoryStore> {
        let keypair = Keypair::generate();
        let node = Node::new(&keypair).expect("a fresh keypair always builds a node");
        NodeState::new(
            node,
            MemoryStore::new(),
            keypair,
            Vec::new(),
            NodeChainMode::RpcConnected {
                rpc_urls: vec!["http://localhost:8899".to_string()],
                ws_url: None,
            },
            openfiat_snapshot::trust::TrustAnchors::pinned(),
        )
    }

    fn register_snapshot_provider(state: &NodeState<MemoryStore>, provider: &Keypair) {
        state
            .services
            .apply_registration(SignedRegistration::sign(
                Registration {
                    service_id: ServiceId::new("snapshot-svc"),
                    service_type: ServiceType::Infrastructure(
                        InfrastructureService::SnapshotProvider,
                    ),
                    provider: peer_id_from_public_key(&provider.public_key()).unwrap(),
                    provider_public_key: provider.public_key(),
                    endpoints: vec![],
                    supported_ofs: vec![1300],
                    region: None,
                    capabilities: vec![],
                    branding: None,
                    pricing: None,
                    payout_wallet: None,
                    timestamp: Timestamp::now(),
                },
                provider,
            ))
            .unwrap();
    }

    #[tokio::test]
    async fn a_staked_provider_is_recorded_from_the_address_this_node_derives() {
        // The fixture is keyed by address, so this only passes if the
        // derived PDA matches — a poll that looked somewhere else reads
        // `None` and records a zero balance.
        let state = rpc_connected_node();
        let provider = Keypair::from_seed([3u8; 32]);
        register_snapshot_provider(&state, &provider);
        let peer = peer_id_from_public_key(&provider.public_key()).unwrap();

        let cluster =
            FixtureCluster::new().with_stake(&provider.public_key(), 6, 25_000_000_000_000);
        poll_provider_stake(&state, &cluster).await;

        assert_eq!(
            state.snapshots.stake_standing(&peer, Timestamp::now()),
            StakeStanding::Qualified
        );
    }

    #[tokio::test]
    async fn a_provider_with_no_stake_account_at_all_is_recorded_as_holding_nothing() {
        // "The cluster says there is no such account" is a fact about the
        // provider, not about the connection, and recording it is what
        // lets an unbacked announcement stop being relayed.
        let state = rpc_connected_node();
        let provider = Keypair::from_seed([4u8; 32]);
        register_snapshot_provider(&state, &provider);
        let peer = peer_id_from_public_key(&provider.public_key()).unwrap();

        poll_provider_stake(&state, &FixtureCluster::new()).await;

        assert_eq!(
            state.snapshots.stake_standing(&peer, Timestamp::now()),
            StakeStanding::Insufficient {
                held: 0,
                required: MINIMUM_PROVIDER_STAKE,
            }
        );
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_does_not_demote_a_provider_it_could_not_read() {
        // The distinction the whole error handling here turns on. An RPC
        // outage is not evidence that anybody unstaked, and a poll that
        // treated it as such would cut a node off from every provider the
        // moment its endpoint hiccuped.
        let state = rpc_connected_node();
        let provider = Keypair::from_seed([5u8; 32]);
        register_snapshot_provider(&state, &provider);
        let peer = peer_id_from_public_key(&provider.public_key()).unwrap();

        poll_provider_stake(
            &state,
            &FixtureCluster::new().with_stake(&provider.public_key(), 6, MINIMUM_PROVIDER_STAKE),
        )
        .await;
        poll_provider_stake(&state, &FixtureCluster::offline()).await;

        assert_eq!(
            state.snapshots.stake_standing(&peer, Timestamp::now()),
            StakeStanding::Qualified,
            "the last good reading must stand until it ages out on its own"
        );
    }

    #[tokio::test]
    async fn governance_can_raise_the_requirement_and_cannot_lower_this_nodes_floor() {
        let state = rpc_connected_node();
        let provider = Keypair::from_seed([6u8; 32]);
        register_snapshot_provider(&state, &provider);
        let peer = peer_id_from_public_key(&provider.public_key()).unwrap();

        // Governance's own SnapshotProvider minimum is the last slot.
        // Deployed at 1,000 OPEN, well under this node's floor.
        let deployed =
            [500, 500, 1_000, 5_000, 1_000, 1_000, 1_000].map(|whole: u64| whole * 1_000_000);
        poll_provider_stake(
            &state,
            &FixtureCluster::new().with_config(deployed).with_stake(
                &provider.public_key(),
                6,
                MINIMUM_PROVIDER_STAKE,
            ),
        )
        .await;
        assert_eq!(
            state.provider_stakes.borrow().required(),
            MINIMUM_PROVIDER_STAKE,
            "a governance minimum below the floor must not lower it"
        );
        assert_eq!(
            state.snapshots.stake_standing(&peer, Timestamp::now()),
            StakeStanding::Qualified
        );

        let mut raised = deployed;
        raised[6] = 50_000 * 1_000_000;
        poll_provider_stake(
            &state,
            &FixtureCluster::new().with_config(raised).with_stake(
                &provider.public_key(),
                6,
                MINIMUM_PROVIDER_STAKE,
            ),
        )
        .await;
        assert_eq!(
            state.snapshots.stake_standing(&peer, Timestamp::now()),
            StakeStanding::Insufficient {
                held: MINIMUM_PROVIDER_STAKE,
                required: 50_000 * 1_000_000,
            },
            "raising the on-chain minimum must raise this gate, with no code change"
        );
    }

    #[tokio::test]
    async fn a_gossip_only_node_records_nothing_however_the_chain_answers() {
        // It has no endpoint in production; this proves that even handed
        // one, it does not quietly start believing readings its own mode
        // says it cannot make. See `openfiat_snapshot::stake`.
        let state = NodeState::new_for_test(MemoryStore::new());
        let provider = Keypair::from_seed([7u8; 32]);
        register_snapshot_provider(&state, &provider);
        let peer = peer_id_from_public_key(&provider.public_key()).unwrap();

        poll_provider_stake(
            &state,
            &FixtureCluster::new().with_stake(&provider.public_key(), 6, MINIMUM_PROVIDER_STAKE),
        )
        .await;

        assert_eq!(
            state.snapshots.stake_standing(&peer, Timestamp::now()),
            StakeStanding::Unenforceable
        );
    }

    #[tokio::test]
    async fn an_account_under_another_role_is_refused_rather_than_counted() {
        // A merchant's stake is a real, program-written StakeAccount at a
        // different PDA. Nothing should be able to land it in the
        // snapshot-provider slot — and if the derivation ever collided,
        // this is what notices.
        let state = rpc_connected_node();
        let provider = Keypair::from_seed([8u8; 32]);
        register_snapshot_provider(&state, &provider);
        let peer = peer_id_from_public_key(&provider.public_key()).unwrap();

        let mut cluster = FixtureCluster::new();
        let (owner, mut data) = {
            let staged =
                FixtureCluster::new().with_stake(&provider.public_key(), 0, MINIMUM_PROVIDER_STAKE);
            staged
                .accounts
                .get(&stake::provider_stake_address(&provider.public_key()))
                .cloned()
                .unwrap()
        };
        data[40] = 0; // Role::Merchant, at the snapshot provider's address
        cluster.accounts.insert(
            stake::provider_stake_address(&provider.public_key()),
            (owner, data),
        );
        poll_provider_stake(&state, &cluster).await;

        assert_eq!(
            state.snapshots.stake_standing(&peer, Timestamp::now()),
            StakeStanding::Unread,
            "an undecodable account is not a reading, and must not become a zero one either"
        );
    }
}
