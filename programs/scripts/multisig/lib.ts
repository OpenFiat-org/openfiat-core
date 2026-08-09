/**
 * Shared helpers for the devnet 3-of-5 Squads multisig migration.
 *
 * The migration moves the BPF *upgrade authority* of every OpenFiat program
 * from the single EA8Ty keypair to a Squads v4 multisig, so that changing any
 * program's code takes three of five independent signers instead of one hot
 * key. This file is the common ground the create/migrate scripts stand on:
 * loading the signer set, deriving the Squads PDAs, building a raw
 * BPFLoaderUpgradeable `SetAuthority`, and driving a proposal through the
 * full propose -> approve x3 -> execute cycle.
 *
 * Everything here targets devnet with keys we hold in full, so a wrong turn is
 * recoverable: as long as we control three signers we can always vote the
 * authority back. The mainnet counterpart of this flow is documented, not run.
 */
import {
  Connection,
  Keypair,
  PublicKey,
  TransactionInstruction,
  TransactionMessage,
} from "@solana/web3.js";
import * as multisig from "@sqds/multisig";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { chmodSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

export const RPC_URL =
  process.env.SOLANA_RPC_URL ?? "https://api.devnet.solana.com";

/** The BPF upgradeable loader — owner of every deployed program account. */
export const BPF_LOADER_UPGRADEABLE = new PublicKey(
  "BPFLoaderUpgradeab1e11111111111111111111111",
);

/** Where the five devnet signer keypairs and the multisig registry live. */
const PROGRAMS_DIR = join(__dirname, "..", "..");
export const SIGNERS_DIR = join(PROGRAMS_DIR, "keys", "multisig-signers");
export const REGISTRY_PATH = join(PROGRAMS_DIR, "multisig-devnet.json");

export function getConnection(): Connection {
  return new Connection(RPC_URL, "confirmed");
}

/** Public devnet RPC throttles bursts; space RPC-heavy steps out politely. */
export const sleep = (ms: number): Promise<void> =>
  new Promise((r) => setTimeout(r, ms));

/** Delay between successive RPC sends, overridable when a faster RPC is used. */
export const STEP_DELAY_MS = Number(process.env.MULTISIG_STEP_DELAY_MS ?? 1200);

/** The EA8Ty deploy/authority keypair — the current authority and fee payer. */
export function loadPayer(): Keypair {
  const path = join(homedir(), ".config", "solana", "id.json");
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))),
  );
}

function loadKeypair(path: string): Keypair {
  return Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))),
  );
}

/**
 * Load the five signer keypairs, generating and persisting any that are
 * missing. Generated keys are written 0600 — on devnet they are throwaway,
 * but they now gate real authority, so they are treated as secrets and are
 * meant to be backed up with `scripts/backup-node-keys.sh`.
 */
export function loadOrCreateSigners(count = 5): Keypair[] {
  mkdirSync(SIGNERS_DIR, { recursive: true });
  const signers: Keypair[] = [];
  for (let i = 1; i <= count; i++) {
    const path = join(SIGNERS_DIR, `signer${i}.json`);
    if (existsSync(path)) {
      signers.push(loadKeypair(path));
    } else {
      const kp = Keypair.generate();
      writeFileSync(path, JSON.stringify(Array.from(kp.secretKey)));
      chmodSync(path, 0o600);
      signers.push(kp);
    }
  }
  return signers;
}

export interface Registry {
  cluster: string;
  createKey: string;
  multisigPda: string;
  vaultPda: string;
  threshold: number;
  members: string[];
  createdAt: string;
}

export function readRegistry(): Registry {
  return JSON.parse(readFileSync(REGISTRY_PATH, "utf8"));
}

export function writeRegistry(r: Registry): void {
  writeFileSync(REGISTRY_PATH, JSON.stringify(r, null, 2) + "\n");
}

/** ProgramData account for a program under the upgradeable loader. */
export function programDataAddress(programId: PublicKey): PublicKey {
  return PublicKey.findProgramAddressSync(
    [programId.toBuffer()],
    BPF_LOADER_UPGRADEABLE,
  )[0];
}

/**
 * Raw BPFLoaderUpgradeable `SetAuthority` (instruction 4). Unlike the checked
 * variant it does not require the new authority to sign, so it can hand a
 * program to a PDA — exactly what the CLI's
 * `--skip-new-upgrade-authority-signer-check` does. `currentAuthority` is the
 * only signer; here that is the multisig vault PDA, which signs by CPI when
 * the Squads program executes the vault transaction.
 */
export function setUpgradeAuthorityIx(
  programId: PublicKey,
  currentAuthority: PublicKey,
  newAuthority: PublicKey,
): TransactionInstruction {
  const data = Buffer.alloc(4);
  data.writeUInt32LE(4, 0); // UpgradeableLoaderInstruction::SetAuthority
  return new TransactionInstruction({
    programId: BPF_LOADER_UPGRADEABLE,
    keys: [
      { pubkey: programDataAddress(programId), isSigner: false, isWritable: true },
      { pubkey: currentAuthority, isSigner: true, isWritable: false },
      { pubkey: newAuthority, isSigner: false, isWritable: false },
    ],
    data,
  });
}

/** On-chain upgrade authority of a program, or null if immutable/none. */
export async function readUpgradeAuthority(
  connection: Connection,
  programId: PublicKey,
): Promise<PublicKey | null> {
  const info = await connection.getAccountInfo(programDataAddress(programId));
  if (!info) return null;
  // ProgramData layout: [u32 tag][u64 slot][option<pubkey> authority][bytecode].
  // tag(4) + slot(8) = 12, then the 1-byte Option discriminator.
  const hasAuthority = info.data[12] === 1;
  if (!hasAuthority) return null;
  return new PublicKey(info.data.subarray(13, 13 + 32));
}

/**
 * Drive one set of instructions through the multisig: create the vault
 * transaction, open a proposal, gather `threshold` approvals, and execute.
 * Returns the execution signature. `members` must contain at least
 * `threshold` signers with Vote+Execute permission; `payer` funds every
 * step so the signer keypairs need no SOL of their own.
 */
export async function executeThroughVault(params: {
  connection: Connection;
  multisigPda: PublicKey;
  payer: Keypair;
  members: Keypair[];
  threshold: number;
  instructions: TransactionInstruction[];
  memo?: string;
}): Promise<string> {
  const { connection, multisigPda, payer, members, threshold, instructions } =
    params;

  const info = await multisig.accounts.Multisig.fromAccountAddress(
    connection,
    multisigPda,
  );
  const transactionIndex = BigInt(info.transactionIndex.toString()) + 1n;
  const [vaultPda] = multisig.getVaultPda({ multisigPda, index: 0 });

  const { blockhash } = await connection.getLatestBlockhash();
  const transactionMessage = new TransactionMessage({
    payerKey: vaultPda,
    recentBlockhash: blockhash,
    instructions,
  });

  // The SDK's rpc helpers send without confirming, but each step reads the
  // multisig state the previous step wrote (the transaction index, the
  // proposal, the vote tally), so every signature must be confirmed before the
  // next call or the chain has not yet seen it. `feePayer` is EA8Ty; the
  // creator pays proposal/transaction rent (the SDK charges the creator).
  const confirm = async (sig: string) => {
    await connection.confirmTransaction(sig, "confirmed");
    await sleep(STEP_DELAY_MS);
  };

  await confirm(
    await multisig.rpc.vaultTransactionCreate({
      connection,
      feePayer: payer,
      rentPayer: members[0].publicKey,
      multisigPda,
      transactionIndex,
      creator: members[0].publicKey,
      vaultIndex: 0,
      ephemeralSigners: 0,
      transactionMessage,
      memo: params.memo,
      signers: [payer, members[0]],
    }),
  );

  await confirm(
    await multisig.rpc.proposalCreate({
      connection,
      feePayer: payer,
      creator: members[0],
      multisigPda,
      transactionIndex,
    }),
  );

  for (let i = 0; i < threshold; i++) {
    await confirm(
      await multisig.rpc.proposalApprove({
        connection,
        feePayer: payer,
        member: members[i],
        multisigPda,
        transactionIndex,
      }),
    );
  }

  const sig = await multisig.rpc.vaultTransactionExecute({
    connection,
    feePayer: payer,
    multisigPda,
    transactionIndex,
    member: members[0].publicKey,
    signers: [payer, members[0]],
  });
  await confirm(sig);
  return sig;
}
