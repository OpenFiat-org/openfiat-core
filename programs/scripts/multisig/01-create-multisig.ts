/**
 * Create the devnet 3-of-5 Squads v4 multisig that will hold OpenFiat's
 * program upgrade authority.
 *
 * Idempotent: if `multisig-devnet.json` already names a multisig that exists
 * on chain, it prints it and exits rather than creating a second one. On first
 * run it generates five signer keypairs (persisted 0600 under
 * `keys/multisig-signers/`), creates the multisig with a 3-of-5 threshold and
 * all five members holding full permissions, then reads the account back and
 * asserts the threshold and member set landed exactly as intended before
 * writing the registry.
 *
 *   npx ts-node scripts/multisig/01-create-multisig.ts
 */
import { PublicKey } from "@solana/web3.js";
import * as multisig from "@sqds/multisig";
import {
  getConnection,
  loadOrCreateSigners,
  loadPayer,
  readRegistry,
  REGISTRY_PATH,
  writeRegistry,
} from "./lib";
import { existsSync } from "node:fs";
import { Keypair } from "@solana/web3.js";

const THRESHOLD = 3;

async function main() {
  const connection = getConnection();
  const payer = loadPayer();
  const signers = loadOrCreateSigners(5);

  if (existsSync(REGISTRY_PATH)) {
    const r = readRegistry();
    const existing = await connection.getAccountInfo(
      new PublicKey(r.multisigPda),
    );
    if (existing) {
      console.log("Multisig already exists — nothing to do.");
      console.log(JSON.stringify(r, null, 2));
      return;
    }
  }

  const createKey = Keypair.generate();
  const [multisigPda] = multisig.getMultisigPda({
    createKey: createKey.publicKey,
  });
  const [vaultPda] = multisig.getVaultPda({ multisigPda, index: 0 });

  const programConfigPda = multisig.getProgramConfigPda({})[0];
  const programConfig =
    await multisig.accounts.ProgramConfig.fromAccountAddress(
      connection,
      programConfigPda,
    );

  const members = signers.map((s) => ({
    key: s.publicKey,
    permissions: multisig.types.Permissions.all(),
  }));

  console.log(`Creating 3-of-5 multisig ${multisigPda.toBase58()} ...`);
  const sig = await multisig.rpc.multisigCreateV2({
    connection,
    treasury: programConfig.treasury,
    createKey,
    creator: payer,
    multisigPda,
    configAuthority: null, // no admin key: config changes also go through a vote
    threshold: THRESHOLD,
    members,
    timeLock: 0,
    rentCollector: null,
    memo: "OpenFiat devnet authority multisig",
  });
  await connection.confirmTransaction(sig, "confirmed");

  // Read it back and prove the on-chain config is exactly what we asked for.
  const account = await multisig.accounts.Multisig.fromAccountAddress(
    connection,
    multisigPda,
  );
  if (account.threshold !== THRESHOLD) {
    throw new Error(`threshold is ${account.threshold}, expected ${THRESHOLD}`);
  }
  if (account.members.length !== signers.length) {
    throw new Error(
      `member count is ${account.members.length}, expected ${signers.length}`,
    );
  }
  const onChain = new Set(account.members.map((m) => m.key.toBase58()));
  for (const s of signers) {
    if (!onChain.has(s.publicKey.toBase58())) {
      throw new Error(`signer ${s.publicKey.toBase58()} missing on chain`);
    }
  }

  writeRegistry({
    cluster: "devnet",
    createKey: createKey.publicKey.toBase58(),
    multisigPda: multisigPda.toBase58(),
    vaultPda: vaultPda.toBase58(),
    threshold: THRESHOLD,
    members: signers.map((s) => s.publicKey.toBase58()),
    createdAt: new Date().toISOString(),
  });

  console.log("Verified 3-of-5 multisig on chain.");
  console.log(`  multisig : ${multisigPda.toBase58()}`);
  console.log(`  vault    : ${vaultPda.toBase58()}  (new upgrade authority)`);
  console.log(`  create tx: ${sig}`);
  console.log(`  registry : ${REGISTRY_PATH}`);
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
