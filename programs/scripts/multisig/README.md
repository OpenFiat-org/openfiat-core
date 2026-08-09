# Authority multisig (3-of-5 Squads)

OpenFiat's on-chain authority — who may upgrade a program's code or act as its
admin — must not be a single hot key. This directory holds the scripts that put
that authority behind a **3-of-5 [Squads](https://squads.so) v4 multisig**, and
the runbook for doing it on mainnet.

The rule the design turns on: **whoever holds a program's BPF upgrade authority
can rewrite that program, and therefore holds every authority the program
enforces.** So the upgrade authority is the one that must move first and matters
most. The in-program `admin`/`treasury` fields matter too, but they cannot be
overridden by anyone who does not already control the code.

## Proven on devnet

`01-create-multisig.ts` then `02-migrate-authorities.ts` were run against devnet
and migrated the upgrade authority of all four programs from the single `EA8Ty`
keypair to the multisig, verified end to end (see `devnet-addresses.json` →
`devnet_authority_multisig`):

| | address |
|---|---|
| Squads program (v4) | `SQDS4ep65T869zMMBKyuUq6aD6EgTu8psMjkvj52pCf` |
| Multisig | `GWHgY9vAX3HDeSVMCDcGFxGJf5yMoKL3RBzY1EYHTaZv` |
| Vault (the new upgrade authority) | `FSKpdbqjxPnPhkH8pmRcxWHYxMCkuR6boiku9MArtkNh` |
| Threshold | 3 of 5 |

`02-migrate-authorities.ts` does not just flip the authority; it earns the right
to. First it moves SOL out of the vault by a 3-of-5 vote (proving the five
signer keys drive propose → approve×3 → execute and that the vault signs by
CPI). Then it hands one program to the vault and votes it straight back to
`EA8Ty` (proving the vault genuinely wields `BPFLoaderUpgradeable::SetAuthority`
and that **recovery works**) before migrating all four. Because we hold all five
devnet signer keys, three of them can always vote any authority back — the
migration is reversible.

## The immutable-admin caveat

Each program writes its `admin` (and presale its `treasury`) **once, in
`initialize_*`**, with no `set_admin` instruction. So on an already-initialized
deployment those fields cannot be handed to the multisig without first shipping
a code change that adds a transfer instruction — itself an upgrade. This is why
the devnet migration moves the **upgrade authority** (which is always
reassignable and transitively controls the rest) and leaves the admin fields on
`EA8Ty`. On mainnet the fields are never on a hot key to begin with: they are
**initialized directly to the vault** (below).

## Scripts

```bash
# Create the multisig (devnet: generates 5 signer keypairs under keys/multisig-signers/)
npx ts-node scripts/multisig/01-create-multisig.ts

# Migrate every program's upgrade authority to the vault, with proofs
npx ts-node scripts/multisig/02-migrate-authorities.ts
```

Environment:
- `SOLANA_RPC_URL` — RPC endpoint (default public devnet). A private RPC avoids
  the 429s the public one throttles bursts with.
- `MULTISIG_STEP_DELAY_MS` — pause between RPC-heavy steps (default `1200`). Drop
  it toward `0` on a private RPC to run faster.

The five signer keypairs are secrets. They live under `keys/multisig-signers/`
(git-ignored) and are copied into the host custody dir
`/root/openfiat-node-keys/multisig-signers/`, so
[`scripts/backup-node-keys.sh`](../backup-node-keys.sh) includes them in its
encrypted, off-machine backup. **Lose three of five and control is lost
permanently** — this is exactly what the backup and hardware custody below guard
against.

## Mainnet runbook

Gated: mainnet program deploy is forbidden until the external audit and OFS-4100
tokenomics sign-off (see the program's `README.md`). Run this only after that
gate is met.

1. **Pick five signers** that fail independently — five people, ideally on five
   hardware wallets, geographically and organizationally separated. Collect
   their five public keys. Three must be reachable to act; three lost keys lose
   everything, so five distinct custodians, not five keys in one drawer.
2. **Create the multisig.** Adapt `01-create-multisig.ts` to take the five real
   pubkeys as `members` (rather than generating throwaway keys) with
   `Permissions.all()`, `threshold: 3`, and `configAuthority: null` (config
   changes also require a vote). Run it against mainnet and confirm the on-chain
   threshold and member set — the script already asserts both.
3. **Set the upgrade authority at deploy time.** Deploy each program with
   `--upgrade-authority <vaultPda>`, or migrate an already-deployed program with
   `solana program set-upgrade-authority <programId> --new-upgrade-authority
   <vaultPda> --skip-new-upgrade-authority-signer-check` signed by the current
   authority. Verify with `solana program show <programId>`.
4. **Initialize the in-program admins to the vault.** Because `admin`/`treasury`
   are init-only, pass the **vault PDA** as `admin` in every `initialize_*`
   (`initialize_sale`, `initialize_fee_config`, `initialize_staking_config`,
   `initialize_governance_config`) and set presale `treasury` to the
   multisig-controlled treasury token account. This is the whole reason the
   mainnet init is a single, careful ceremony — there is no second chance to set
   these without an upgrade.
5. **Fund a proposing signer** (or a dedicated rent payer) with SOL: the Squads
   SDK charges the proposal/transaction-account rent to the proposal *creator*,
   not the fee payer.
6. **Operate by vote.** Every upgrade and every admin instruction is now a
   Squads proposal needing three approvals — through [app.squads.so](https://app.squads.so)
   or the `executeThroughVault` path in `lib.ts`. An admin instruction executes
   with the vault PDA signing by CPI, which is why the programs take their
   authorities as plain `Pubkey`s a PDA can hold.
7. **Recovery and rotation.** With three signers you can vote to replace a lost
   member (`multisigAddMember`/`multisigRemoveMember`) or move an authority.
   Below three, control is unrecoverable — so keep the [encrypted backups](../backup-node-keys.sh)
   current and the hardware keys distributed.
