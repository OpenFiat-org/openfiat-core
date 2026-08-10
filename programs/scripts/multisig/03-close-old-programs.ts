/**
 * Close the four superseded OpenFiat devnet programs through the 3-of-5
 * multisig, reclaiming their rent-exempt SOL to EA8Ty.
 *
 * Part of the 2026-08-09 tokenomics re-baseline "fresh full redeploy": the
 * pre-re-baseline programs (old IDs below) are being abandoned for a new
 * deployment on new IDs against a new 6-decimal / 100B OPEN mint. Their
 * ProgramData rent (~16 SOL total) is locked behind the multisig vault (their
 * upgrade authority), so it can only be recovered by a vault-authorized
 * `BPFLoaderUpgradeable::Close`, driven propose -> approve x3 -> execute.
 *
 * IRREVERSIBLE: a closed program id can never be redeployed. Run only once the
 * new deployment is intended to replace these permanently.
 *
 *   npx ts-node scripts/multisig/03-close-old-programs.ts
 */
import { Connection, PublicKey, TransactionInstruction } from "@solana/web3.js";
import {
  BPF_LOADER_UPGRADEABLE,
  executeThroughVault,
  getConnection,
  loadOrCreateSigners,
  loadPayer,
  programDataAddress,
  readRegistry,
  sleep,
  STEP_DELAY_MS,
} from "./lib";

/** The pre-re-baseline (old) program ids being retired. */
const OLD_PROGRAMS: Record<string, PublicKey> = {
  presale: new PublicKey("75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7"),
  escrow: new PublicKey("HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP"),
  staking: new PublicKey("HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx"),
  governance: new PublicKey("AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE"),
};

/**
 * Raw `BPFLoaderUpgradeable::Close` (instruction 5) for a program: closes its
 * ProgramData account, sending the reclaimed lamports to `recipient`. The
 * `authority` is the only signer — here the multisig vault, signing by CPI.
 * Account order matches the loader's `close_any(programdata, recipient,
 * Some(authority), Some(program))`.
 */
function closeProgramIx(
  programId: PublicKey,
  recipient: PublicKey,
  authority: PublicKey,
): TransactionInstruction {
  const data = Buffer.alloc(4);
  data.writeUInt32LE(5, 0); // UpgradeableLoaderInstruction::Close
  return new TransactionInstruction({
    programId: BPF_LOADER_UPGRADEABLE,
    keys: [
      { pubkey: programDataAddress(programId), isSigner: false, isWritable: true },
      { pubkey: recipient, isSigner: false, isWritable: true },
      { pubkey: authority, isSigner: true, isWritable: false },
      { pubkey: programId, isSigner: false, isWritable: true },
    ],
    data,
  });
}

async function programDataExists(
  connection: Connection,
  programId: PublicKey,
): Promise<boolean> {
  return (await connection.getAccountInfo(programDataAddress(programId))) !== null;
}

async function main() {
  const connection = getConnection();
  const payer = loadPayer(); // EA8Ty — recipient of reclaimed rent + fee payer
  const members = loadOrCreateSigners(5);
  const r = readRegistry();
  const multisigPda = new PublicKey(r.multisigPda);
  const vaultPda = new PublicKey(r.vaultPda);

  console.log(`Multisig : ${multisigPda.toBase58()} (${r.threshold}-of-${r.members.length})`);
  console.log(`Vault    : ${vaultPda.toBase58()} (close authority)`);
  console.log(`Recipient: ${payer.publicKey.toBase58()} (EA8Ty)`);

  const before = await connection.getBalance(payer.publicKey);
  console.log(`EA8Ty balance before: ${(before / 1e9).toFixed(4)} SOL\n`);

  for (const [name, programId] of Object.entries(OLD_PROGRAMS)) {
    if (!(await programDataExists(connection, programId))) {
      console.log(`  · ${name} (${programId.toBase58()}) already closed — skipping`);
      continue;
    }
    console.log(`Closing ${name} (${programId.toBase58()}) by 3-of-5 vote...`);
    const ix = closeProgramIx(programId, payer.publicKey, vaultPda);
    const sig = await executeThroughVault({
      connection,
      multisigPda,
      payer,
      members,
      threshold: r.threshold,
      instructions: [ix],
      memo: `close old ${name}`,
    });
    console.log(`  ✓ ${name} closed (${sig})`);
    await sleep(STEP_DELAY_MS);
  }

  const after = await connection.getBalance(payer.publicKey);
  console.log(
    `\nEA8Ty balance after: ${(after / 1e9).toFixed(4)} SOL (reclaimed ~${((after - before) / 1e9).toFixed(4)} SOL)`,
  );
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
