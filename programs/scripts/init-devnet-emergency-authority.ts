/**
 * Create the `EmergencyAuthority` singleton on devnet.
 *
 * The governance sunset (#121, OFS-4100 §5.1) lives on a new account
 * rather than as fields on `GovernanceConfig`, because widening a live
 * Anchor `#[account]` makes every already-allocated account of that type
 * undeserializable and devnet holds a live config. The consequence is
 * that after the program upgrade the account does not exist yet, and
 * `update_governance_config` fails on the missing account until it does.
 * This creates it once.
 *
 * The instruction takes **no arguments** and has no authority. Both
 * holder keys and the first-year deadline are written by the program
 * from its own constants, and `expires_at` has no writer afterwards —
 * that is what makes the sunset non-extendable rather than merely
 * unextended. So this script cannot get the parameters wrong: there are
 * none to pass. Its only job is to pay rent.
 *
 * Permissionless by design, per §5.1: if creation required a key, the
 * holder of that key could stall the clock by declining to start it, and
 * a deadline nobody can start is not a deadline.
 *
 * Idempotent. Anchor's `init` fails with "already in use" on a second
 * run; this checks first and exits successfully rather than reporting a
 * failure for a state that is already correct.
 */
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const GOVERNANCE = new PublicKey("2k71DBDoxM4SUFYGbyMXFiTSUynPuY2CqFUsx3FuarXF");
const EMERGENCY_AUTHORITY_SEED = Buffer.from("emergency_authority");

/**
 * Anchor's global instruction discriminator: `sha256("global:<name>")[..8]`.
 *
 * Computed rather than transcribed. A hand-copied discriminator is a
 * second place for the layout to drift, and this program's own IDL-drift
 * test exists because that has happened here before.
 */
function discriminator(name: string): Buffer {
  return createHash("sha256").update(`global:${name}`).digest().subarray(0, 8);
}

async function main() {
  const rpc = process.env.ANCHOR_PROVIDER_URL ?? "https://api.devnet.solana.com";
  if (rpc.includes("mainnet")) {
    throw new Error("this script is devnet-only; refusing a mainnet endpoint");
  }
  const walletPath =
    process.env.ANCHOR_WALLET ?? `${process.env.HOME}/.config/solana/id.json`;
  const payer = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(walletPath, "utf8"))),
  );
  const connection = new Connection(rpc, "confirmed");

  const [pda, bump] = PublicKey.findProgramAddressSync(
    [EMERGENCY_AUTHORITY_SEED],
    GOVERNANCE,
  );
  console.log(`governance program : ${GOVERNANCE.toBase58()}`);
  console.log(`emergency authority: ${pda.toBase58()} (bump ${bump})`);
  console.log(`payer              : ${payer.publicKey.toBase58()}`);

  const existing = await connection.getAccountInfo(pda);
  if (existing) {
    console.log(
      `\nalready initialized — ${existing.data.length} bytes, owner ${existing.owner.toBase58()}`,
    );
    console.log("nothing to do.");
    return;
  }

  const ix = new TransactionInstruction({
    programId: GOVERNANCE,
    keys: [
      { pubkey: payer.publicKey, isSigner: true, isWritable: true },
      { pubkey: pda, isSigner: false, isWritable: true },
      { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
    ],
    data: discriminator("initialize_emergency_authority"),
  });

  const signature = await connection.sendTransaction(
    new Transaction().add(ix),
    [payer],
    { preflightCommitment: "confirmed" },
  );
  const latest = await connection.getLatestBlockhash("confirmed");
  await connection.confirmTransaction(
    { signature, ...latest },
    "confirmed",
  );
  console.log(`\ncreated. signature ${signature}`);

  // Read it back. A send that confirmed proves the transaction landed,
  // not that the account it was supposed to create exists and is owned
  // by the program that should own it.
  const created = await connection.getAccountInfo(pda);
  if (!created) {
    throw new Error("transaction confirmed but the account does not exist");
  }
  if (!created.owner.equals(GOVERNANCE)) {
    throw new Error(`account is owned by ${created.owner.toBase58()}, not governance`);
  }
  console.log(`verified: ${created.data.length} bytes, owned by governance`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
