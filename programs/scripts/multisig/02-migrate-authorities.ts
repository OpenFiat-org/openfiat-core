/**
 * Migrate the BPF upgrade authority of every OpenFiat devnet program to the
 * 3-of-5 multisig created by 01-create-multisig.ts.
 *
 * The order is deliberately paranoid, because handing a program to an
 * authority we cannot drive would strand it:
 *
 *   1. Plumbing proof (no authority at risk): fund the vault a little SOL and
 *      move some back out through a 3-of-5 vote. Proves our five signer keys
 *      actually drive propose -> approve x3 -> execute and that the vault can
 *      sign by CPI.
 *   2. Loader proof (one program, fully reversible): migrate escrow to the
 *      vault, then vote it straight back to EA8Ty. Proves the vault genuinely
 *      wields BPFLoaderUpgradeable SetAuthority — the exact power the real
 *      migration depends on — and proves recovery works.
 *   3. Migration: hand all four programs to the vault, asserting each landed.
 *
 * Every authority change is read back off chain and asserted. Reversible
 * throughout: we hold all five signers, so three of them can always vote any
 * program's authority back. Idempotent per program — a program already on the
 * vault is skipped.
 *
 *   npx ts-node scripts/multisig/02-migrate-authorities.ts
 */
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import {
  executeThroughVault,
  getConnection,
  loadOrCreateSigners,
  loadPayer,
  readRegistry,
  readUpgradeAuthority,
  setUpgradeAuthorityIx,
  sleep,
  STEP_DELAY_MS,
} from "./lib";

const PROGRAMS: Record<string, PublicKey> = {
  presale: new PublicKey("75rJ9MRAaSnAc8tg4AfeTFVDCVrN6jdD5CqeyE4UoUw7"),
  escrow: new PublicKey("HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP"),
  staking: new PublicKey("HYEXk8XQukBkZbiYB33JyVefQDxqyCpPudad3wBCyYmx"),
  governance: new PublicKey("AVJfKUjHsizkGGUy8sdz4Xma2hVgmgvgg8GmUMs8E4eE"),
};

/** Send an EA8Ty-signed SetAuthority moving `programId` to `newAuthority`. */
async function setAuthorityDirect(
  connection: Connection,
  payer: Keypair,
  programId: PublicKey,
  newAuthority: PublicKey,
): Promise<string> {
  const ix = setUpgradeAuthorityIx(programId, payer.publicKey, newAuthority);
  const tx = new Transaction().add(ix);
  const { blockhash, lastValidBlockHeight } =
    await connection.getLatestBlockhash();
  tx.recentBlockhash = blockhash;
  tx.feePayer = payer.publicKey;
  tx.sign(payer);
  const sig = await connection.sendRawTransaction(tx.serialize());
  await connection.confirmTransaction(
    { signature: sig, blockhash, lastValidBlockHeight },
    "confirmed",
  );
  await sleep(STEP_DELAY_MS);
  return sig;
}

async function assertAuthority(
  connection: Connection,
  programId: PublicKey,
  expected: PublicKey,
  label: string,
): Promise<void> {
  const actual = await readUpgradeAuthority(connection, programId);
  if (!actual || !actual.equals(expected)) {
    throw new Error(
      `${label}: authority is ${actual?.toBase58() ?? "none"}, expected ${expected.toBase58()}`,
    );
  }
  console.log(`  ✓ ${label} authority = ${expected.toBase58()}`);
  await sleep(STEP_DELAY_MS);
}

async function plumbingProof(
  connection: Connection,
  payer: Keypair,
  multisigPda: PublicKey,
  vaultPda: PublicKey,
  members: Keypair[],
  threshold: number,
): Promise<void> {
  console.log("\n[1/3] Plumbing proof — moving SOL through a 3-of-5 vote");
  const fund = 0.02 * 1e9;
  const move = 0.005 * 1e9;

  const fundTx = new Transaction().add(
    SystemProgram.transfer({
      fromPubkey: payer.publicKey,
      toPubkey: vaultPda,
      lamports: fund,
    }),
  );
  const { blockhash, lastValidBlockHeight } =
    await connection.getLatestBlockhash();
  fundTx.recentBlockhash = blockhash;
  fundTx.feePayer = payer.publicKey;
  fundTx.sign(payer);
  const fundSig = await connection.sendRawTransaction(fundTx.serialize());
  await connection.confirmTransaction(
    { signature: fundSig, blockhash, lastValidBlockHeight },
    "confirmed",
  );

  const before = await connection.getBalance(vaultPda);
  const transferOut: TransactionInstruction = SystemProgram.transfer({
    fromPubkey: vaultPda,
    toPubkey: payer.publicKey,
    lamports: move,
  });
  const sig = await executeThroughVault({
    connection,
    multisigPda,
    payer,
    members,
    threshold,
    instructions: [transferOut],
    memo: "plumbing proof",
  });
  await connection.confirmTransaction(sig, "confirmed");
  const after = await connection.getBalance(vaultPda);
  if (before - after !== move) {
    throw new Error(
      `vault moved ${before - after} lamports, expected ${move}`,
    );
  }
  console.log(`  ✓ vault debited exactly ${move} lamports by vote (${sig})`);
}

async function loaderProof(
  connection: Connection,
  payer: Keypair,
  multisigPda: PublicKey,
  vaultPda: PublicKey,
  members: Keypair[],
  threshold: number,
): Promise<void> {
  console.log("\n[2/3] Loader proof — escrow to vault and back by vote");
  const escrow = PROGRAMS.escrow;

  // Idempotent to a partial prior run: only do the forward hand-off if EA8Ty
  // still holds escrow. Either way we finish by voting it back, which is the
  // load-bearing proof — that the vault genuinely wields SetAuthority.
  const start = await readUpgradeAuthority(connection, escrow);
  if (start && start.equals(payer.publicKey)) {
    await setAuthorityDirect(connection, payer, escrow, vaultPda);
    await assertAuthority(connection, escrow, vaultPda, "escrow -> vault");
  } else if (start && start.equals(vaultPda)) {
    console.log("  · escrow already on the vault (prior run) — proving recovery");
  } else {
    throw new Error(`escrow authority is ${start?.toBase58() ?? "none"}, cannot run proof`);
  }

  const back = setUpgradeAuthorityIx(escrow, vaultPda, payer.publicKey);
  const sig = await executeThroughVault({
    connection,
    multisigPda,
    payer,
    members,
    threshold,
    instructions: [back],
    memo: "loader proof: escrow back to EA8Ty",
  });
  await assertAuthority(connection, escrow, payer.publicKey, "escrow -> EA8Ty");
  console.log(`  ✓ vault wields SetAuthority and recovery works (${sig})`);
}

async function main() {
  const connection = getConnection();
  const payer = loadPayer();
  const members = loadOrCreateSigners(5);
  const r = readRegistry();
  const multisigPda = new PublicKey(r.multisigPda);
  const vaultPda = new PublicKey(r.vaultPda);

  console.log(`Multisig : ${multisigPda.toBase58()} (${r.threshold}-of-${r.members.length})`);
  console.log(`Vault    : ${vaultPda.toBase58()} (target upgrade authority)`);

  // The proposing signer pays the rent for each proposal/transaction account
  // (the Squads SDK charges the creator, not the fee payer). Top it up once so
  // the votes below don't stall — real signers hold their own SOL likewise.
  const creator = members[0];
  if ((await connection.getBalance(creator.publicKey)) < 0.05 * 1e9) {
    const top = new Transaction().add(
      SystemProgram.transfer({
        fromPubkey: payer.publicKey,
        toPubkey: creator.publicKey,
        lamports: 0.15 * 1e9,
      }),
    );
    const bh = await connection.getLatestBlockhash();
    top.recentBlockhash = bh.blockhash;
    top.feePayer = payer.publicKey;
    top.sign(payer);
    const s = await connection.sendRawTransaction(top.serialize());
    await connection.confirmTransaction(
      { signature: s, blockhash: bh.blockhash, lastValidBlockHeight: bh.lastValidBlockHeight },
      "confirmed",
    );
    console.log(`Funded proposing signer ${creator.publicKey.toBase58()} with 0.15 SOL`);
  }

  await plumbingProof(connection, payer, multisigPda, vaultPda, members, r.threshold);
  await loaderProof(connection, payer, multisigPda, vaultPda, members, r.threshold);

  console.log("\n[3/3] Migration — handing every program to the vault");
  for (const [name, programId] of Object.entries(PROGRAMS)) {
    const current = await readUpgradeAuthority(connection, programId);
    if (current && current.equals(vaultPda)) {
      console.log(`  · ${name} already on the vault — skipping`);
      continue;
    }
    if (!current || !current.equals(payer.publicKey)) {
      throw new Error(
        `${name}: authority is ${current?.toBase58() ?? "none"}, refusing to migrate what EA8Ty does not hold`,
      );
    }
    await setAuthorityDirect(connection, payer, programId, vaultPda);
    await assertAuthority(connection, programId, vaultPda, `${name} -> vault`);
  }

  console.log("\nFinal verification — all authorities on the vault:");
  for (const [name, programId] of Object.entries(PROGRAMS)) {
    await assertAuthority(connection, programId, vaultPda, name);
  }
  console.log(
    "\nDone. Every program's upgrade authority is now the 3-of-5 vault.",
  );
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
