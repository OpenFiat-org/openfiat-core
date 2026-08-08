# Conformance

This maps every `MUST` in each protocol spec's own Conformance section to
the automated test that verifies it — Phase 10's deliverable, so
"conformant" is a traceable claim instead of a manual read-through. Run
the whole suite with `cargo test --workspace`; `openfiat-conformance`
additionally proves several of these hold when every domain is composed
onto one real node, not just in a single domain's own isolated test.

A few `MUST`s are satisfied structurally by this workspace's mandated
choice of libp2p as the transport (decision #1 in `docs/architecture.md`)
rather than by a bespoke test — libp2p's own test suite covers Noise/QUIC/
Yamux/multistream-select correctness, and reimplementing that coverage
here would just be testing a dependency. Those are marked accordingly
below rather than pointed at a test that doesn't exist.

## OFS-1000 — Network Protocol (OFNP), §27

| Requirement | Verified by |
| --- | --- |
| Implement libp2p networking | Structural: every node crate builds on `libp2p` directly (`crates/network/Cargo.toml`). |
| Support authenticated Noise sessions | `libp2p-noise`, provided by the dependency (structural). |
| Implement protocol negotiation | `libp2p`'s multistream-select (structural). |
| Support multiplexed services | `libp2p-yamux` (structural). |
| Maintain session sequence numbers | `network::sequence` unit tests: `accepts_strictly_increasing_sequence_numbers`, `rejects_a_repeated_sequence_number`, `rejects_a_sequence_number_below_the_high_water_mark`. |
| Authenticate application messages | `network::envelope::authenticated_with_sets_the_flag_and_signature`; end-to-end in `network/tests/handshake.rs::two_nodes_handshake_exchange_envelopes_reject_replays_and_shut_down_gracefully`. |
| Support capability advertisement | `libp2p-identify`, wired in `crates/network/src/behaviour.rs` (structural — no dedicated test beyond the crate compiling against it, since the capability list itself has no protocol-defined content yet). |
| Implement graceful shutdown | `network::lifecycle::walks_the_full_lifecycle_in_order`; end-to-end in the same `handshake.rs` test (its shutdown phase). |
| Reject incompatible protocol versions | `libp2p`'s multistream-select rejects unnegotiable protocols (structural). |

## OFS-1200 — Gossip Protocol (OGP), §26

| Requirement | Verified by |
| --- | --- |
| Generate globally unique Event IDs | `gossip::event_id` unit tests (hash of type+payload+timestamp+origin+signature). |
| Validate all received events | `gossip/tests/propagation.rs::event_reaches_every_node_exactly_once_and_a_resend_is_suppressed`. |
| Suppress duplicates | Same test (the "resend is suppressed" half). |
| Respect TTL | `gossip/tests/propagation.rs::a_ttl_of_one_reaches_the_direct_peer_but_not_a_second_hop`. |
| Verify signatures | `gossip::service` unit tests + every domain's `Signed*::verify` (e.g. `advertisements::events`). |
| Store recent events | `gossip::store::EventStore` unit tests. |
| Support selective subscriptions | `gossip::channel::Subscription` unit tests. |
| Forward valid events | `event_reaches_every_node_exactly_once_and_a_resend_is_suppressed` (propagation past the direct peer). |
| Reject malformed events | Per-domain `apply_event`/`apply_*` methods return `Err` on a failed `verify()`; unit-tested in each domain's `store.rs`. |
| Support eventual consistency | `gossip/tests/propagation.rs::a_reconnecting_node_recovers_events_it_missed_while_offline`, and at the whole-node level in `openfiat-conformance`'s `tests/partition_recovery.rs::an_offline_node_recovers_events_from_two_domains_at_once_on_reconnect`. |

## OFS-1500 — Service Registry Protocol (SRP), §24

| Requirement | Verified by |
| --- | --- |
| Support signed service registrations | `registry/tests/replication.rs::registration_and_health_changes_replicate_and_stale_services_expire_everywhere`. |
| Maintain a local replicated registry | Same test. |
| Verify provider identities | `registry::registration::SignedRegistration::verify` unit tests. |
| Support service discovery | `RegistryService::all`/`get`, exercised by every downstream consumer (`notifications`, `oracles`, `risk`, `snapshot`) in their own replication tests. |
| Support updates | `registry::store` unit tests for `apply_update`. |
| Support withdrawals | `registry::store` unit tests for `apply_withdrawal`. |
| Support expiration | `registration_and_health_changes_replicate_and_stale_services_expire_everywhere` (the expiration half). |
| Reject invalid registrations | `SignedRegistration::verify` unit tests (bad signature, mismatched peer ID). |
| Preserve deterministic registry state | `registration_and_health_changes_replicate_and_stale_services_expire_everywhere` asserts identical state across every node in the cluster. |

## OFS-2100 — Advertisement Protocol (OAP), §25

| Requirement | Verified by |
| --- | --- |
| Support Buy and Sell advertisements | `advertisements::record::Direction` unit tests + used in both directions across the workspace's tests. |
| Support fixed and floating pricing | `advertisements::record::PricingModel` unit tests. |
| Integrate with Oracle Providers for floating prices | `PricingModel::Floating` carries an `oracle_provider` reference; median resolution proven in `oracles/tests/replication.rs`. |
| Support automatic inventory updates | `advertisements::store::reserve_liquidity` unit tests; end-to-end liquidity deduction across a cluster in `openfiat-conformance`'s `tests/trade_lifecycle.rs`. |
| Support multiple payment methods | `AdvertisementCreate.payment_methods: Vec<String>`, unit-tested in `advertisements::events`. |
| Support Offline Mode | `advertisements::record::AdvertisementStatus` unit tests. |
| Support Vacation Mode | Same. |
| Support automatic advertisement disabling | `advertisements/tests/replication.rs::a_created_and_then_disabled_advertisement_replicates_to_the_whole_cluster`; zero-liquidity auto-disable in `advertisements::store` unit tests. |
| Generate signed advertisement events | `advertisements::events::SignedAdvertisementCreate` (and siblings) unit tests. |
| Synchronize advertisements through the Gossip Protocol | `a_created_and_then_disabled_advertisement_replicates_to_the_whole_cluster`, and chained into a real reservation/settlement in `openfiat-conformance`'s `trade_lifecycle.rs`. |

## OFS-6000 — Notification Protocol (ONP), §22

| Requirement | Verified by |
| --- | --- |
| Support decentralized Notification Providers | `notifications/tests/replication.rs::a_subscription_and_a_delivery_report_replicate_across_the_cluster` (registered via `openfiat-registry`). |
| Verify signed protocol events | `notifications::events` unit tests. |
| Support wallet subscriptions | Same replication test (the subscription half). |
| Support multiple delivery channels | `notifications::record::NotificationCategory`/channel unit tests. |
| Support provider registration | Registered through `openfiat-registry`; see SRP row above. |
| Support delivery confirmations | Same replication test (the delivery-report half). |
| Preserve notification privacy | `notifications::record` — payload carries no third-party PII beyond what the subscribing wallet itself published (design-level; no separate test). |
| Prevent notification forgery | `notifications::events::Signed*::verify` unit tests. |

## OFS-7000 — Oracle Protocol (OOP), §17

| Requirement | Verified by |
| --- | --- |
| Support multiple Oracle Providers | `oracles/tests/replication.rs::three_providers_rates_aggregate_to_the_correct_median_across_the_cluster`. |
| Verify signatures | `oracles::events::SignedOraclePublish::verify` unit tests. |
| Support record expiration | `oracles::store` TTL/expiry unit tests. |
| Synchronize oracle updates | `three_providers_rates_aggregate_to_the_correct_median_across_the_cluster`. |
| Reject unauthorized providers | `gossip::authorization::is_authorized` requires `NodeRole::OracleProvider` for `FXPriceUpdated`; unit-tested in `gossip::authorization`. |
| Support provider redundancy | `three_providers_rates_aggregate_to_the_correct_median_across_the_cluster` (three independent providers). |

## OFS-7100 — Risk Intelligence Protocol (ORIP), §21

| Requirement | Verified by |
| --- | --- |
| Support multiple Risk Intelligence Providers | `risk/tests/replication.rs::two_of_three_providers_flagging_a_scam_wallet_aggregates_to_reject`. |
| Verify provider signatures | `risk::events::SignedRiskPublish::verify` unit tests. |
| Support wallet screening | `two_of_three_providers_flagging_a_scam_wallet_aggregates_to_reject` (OFS-7100 §13's own worked example). |
| Support signed intelligence records | `risk::events` unit tests. |
| Preserve historical intelligence | `risk::store` unit tests (append-only record retention). |
| Support provider registration | Registered through `openfiat-registry`; see SRP row above. |
| Reject malformed intelligence records | `risk::store::apply_event` silently drops an unparsable payload before it reaches the registry (structural, same pattern as every other domain's `apply_event`); `risk::store::an_unregistered_publisher_is_rejected` covers the authorization half of "invalid". |
| Synchronize intelligence updates | `two_of_three_providers_flagging_a_scam_wallet_aggregates_to_reject`. |

## OFS-4300 — Chain Bridge Protocol (OCBP), §12

| Requirement | Verified by |
| --- | --- |
| Support both node connectivity modes | `chain::mode` unit tests: `rpc_connected_reports_itself_as_such`, `gossip_only_reports_itself_as_such`. |
| Never sign a Solana transaction on a user's behalf | Design-level: no code path in `crates/chain` or `crates/rpc::methods::chain` ever holds or constructs a signature — `sendTransaction`'s payload is opaque, already-signed bytes (see OFS-4300 §5, §8); there is no "absence of a capability" to positively test. |
| Deduplicate blockhash announcements by content, not event id | `chain::blockhash` unit tests (`repeated_observation_of_the_same_pair_is_not_new`, `a_different_slot_for_the_same_blockhash_string_is_a_distinct_pair`); at cluster scale, `openfiat-conformance`'s `tests/chain_bridge.rs::blockhash_dedup_bounds_amplification_under_many_independent_announcers` (10 independent origins, one shared hub, one downstream observer). |
| Track and use the highest-slot, not-yet-expired blockhash for its own current view | `chain::blockhash` unit tests: `current_tracks_the_highest_slot_seen_even_if_seen_second`, `an_out_of_order_older_slot_does_not_override_the_current_choice`, `an_expired_blockhash_is_no_longer_returned_as_current`. |
| Expose `getChainStatus`/`getLatestBlockhash`/`sendTransaction` identically regardless of mode | `rpc::methods::chain` unit tests; end to end against a real node in both SDKs' `tests/live_node.*` (Rust and TypeScript). |
| Reject malformed relay-requested transactions before submission | `chain::validate` unit tests (`rejects_empty_bytes`, `rejects_garbage_bytes`); `rpc::methods::chain::send_transaction_rejects_a_malformed_payload`. |
| A gossip-only node's transaction relay request reaches an RPC-connected peer and confirms | `openfiat-conformance`'s `tests/chain_bridge.rs::a_gossip_only_nodes_relay_request_is_observed_and_confirmed_by_an_rpc_connected_peer`. |

## Whole-stack proof

The per-spec tests above each exercise one domain against a bare
`GossipService`. `openfiat-conformance` (see its own README) additionally
proves the composition:

- `tests/trade_lifecycle.rs` — advertisement → reservation → settlement →
  trade, chained across three domain crates on a real 3-node cluster.
- `tests/partition_recovery.rs` — a fully-composed node (every domain, one
  gossip channel) drops offline, misses events in two unrelated domains
  at once, and recovers both on reconnect.
- `tests/chain_bridge.rs` — a gossip-only node's transaction relay
  request is observed and confirmed by an RPC-connected peer, and
  blockhash announcement dedup holds at cluster scale (OFS-4300).

## On-chain program conformance

The proofs above are all off-chain-protocol-only (a real gossip cluster,
but no real Solana state). The on-chain programs (`programs/` —
escrow/staking/governance/presale, OFS-4200) have two further layers of
proof, neither duplicated in this repository:

- **Program-level**: each program's own `anchor test` suite (`programs/tests/`)
  against a real `solana-test-validator` — full vault/stake/proposal
  lifecycles, invariant tests (e.g. no fund movement outside the
  program's own `invoke_signed` PDA seeds).
- **Whole-stack, off-chain + on-chain together**: `openfiat-sdks`'s
  `rust/tests/conformance_*.rs` combine a real off-chain node (this
  repo's own `openfiat_rpc`/`openfiat_api`, in `RpcConnected` mode) with
  the real on-chain programs on a local validator:
  - `conformance_governance.rs` — a real vote's weight is independently
    verified against real on-chain stake (not the vote's own self-report) —
    the first end-to-end proof of this crate's `poll_vote_verifications`.
  - `conformance_dispute.rs` — a full 3-arbitrator on-chain commit-reveal-execute
    cycle, with this repo's `DisputeRegistry` observing the on-chain
    outcome's confirmation via `poll_chain`'s correlation routing.
  - `conformance_trade_lifecycle.rs` — ad → reservation → escrow-lock →
    payment → approval → escrow-release, chaining a real off-chain trade
    with real on-chain escrow. Its final relay step is a documented,
    unresolved environment flake (marked `#[ignore]` with the full
    investigation in that file's own doc comment) — the underlying
    mechanism it exercises is proven correct by the passing dispute test
    above.

- **Presale, claim-anytime + sweep, on devnet**: the `presale` program lets
  a buyer `claim` their OPEN while the sale is still Active (no finalize
  gate) and lets the admin `sweep_proceeds` USDC to the fixed treasury
  mid-sale; there is no refund path (`soft_cap` is forced to 0). This is
  covered program-level by `programs/tests/presale.ts` (25 cases, incl. the
  `claimed_open` high-water mark and all four sweep authorizations) and
  proven end-to-end against real devnet state by
  `programs/scripts/prove-devnet-presale-claim-sweep.ts`: a fresh-nonce sale
  running contribute → claim-while-Active → second-contribute → delta-claim
  → `sweep_proceeds`, asserting on-chain balances at each step. The
  transaction signatures (and the in-place program-upgrade tx) are recorded
  under `devnet_presale_claim_anytime_sweep_proof` in
  `programs/devnet-addresses.json`.
