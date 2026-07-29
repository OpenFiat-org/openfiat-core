# Roadmap — openfiat-core

This document tracks the near-term and long-term direction of this
repository. It is a living document and will be revised as the OpenFiat
protocol matures. See [openfiat-specs](https://github.com/OpenFiat-org/openfiat-specs)
for the canonical protocol roadmap (Chapter 26, Roadmap & Future Vision).

## Status

**Phase: Devnet-complete, mainnet-gated.** Every domain crate (types
through `api`/`cli`) has real, tested business logic — advertisements,
reservations, settlement, disputes, governance, notifications, oracles,
risk, snapshots, the JSON-RPC/REST surface — over a real libp2p gossip
mesh. The Solana chain bridge (`crates/chain`, OFS-4300) and all three
on-chain programs (`programs/` — escrow, staking, governance, OFS-4200)
are built, deployed to devnet, and exercised end to end (see
[`CONFORMANCE.md`](CONFORMANCE.md) for exactly what's proven and
against what). Mainnet deployment of the on-chain programs is explicitly
gated on an external security audit and final sign-off on
[OFS-4100](https://github.com/OpenFiat-org/openfiat-specs) — see
`programs/README.md`'s own devnet-only banner.

## Near term

- [x] Land core architecture / directory structure
- [x] Wire up CI to green on `main`
- [x] Implement every domain crate with real, tested logic
- [x] Deploy escrow/staking/governance to devnet and prove full
      trade/dispute/governance lifecycles against real on-chain state
- [ ] Publish first `0.1.0` pre-release
- [ ] Real, non-scaffold implementations for `explorer/api`
      (see `openfiat-apps`, which is otherwise deprecated in favor of
      building explorer functionality directly into `openfiat-app`)

## Mid term

- [ ] External security audit of the on-chain programs, ahead of any
      mainnet deployment decision
- [ ] Stabilize public interfaces referenced by other OpenFiat-org repositories
- [ ] Expand automated test coverage further — adversarial/fuzzing
      scenarios beyond the current conformance suite (see
      `openfiat-devtools/fuzzing`)
- [ ] Publish versioned documentation

## Long term

- [ ] Reach API/protocol stability commitments appropriate for a `Core`
      component
- [ ] Progressive decentralization of maintainership per the OpenFiat
      governance process (see `openfiat-governance`)

Have a proposal for this roadmap? Open an issue using the **RFC** template.
