/**
 * Vote (3-of-5) to move the presale program's upgrade authority from the
 * multisig vault back to EA8Ty, so a routine `solana program extend` +
 * upgrade can be done with the single deploy key, after which authority is
 * handed straight back to the vault (a direct EA8Ty-signed SetAuthority, no
 * vote needed). This is the same reversible pattern the SP2 loader proof used.
 *
 *   npx ts-node scripts/multisig/release-presale-to-ea8ty.ts
 */
import { PublicKey } from "@solana/web3.js";
import {
  executeThroughVault,
  getConnection,
  loadOrCreateSigners,
  loadPayer,
  readRegistry,
  readUpgradeAuthority,
  setUpgradeAuthorityIx,
} from "./lib";

const PRESALE = new PublicKey("7KaEpDzZuqye1xqqp3RnvBJXnDxbU3W9zVrUr5vBS2fU");

async function main() {
  const connection = getConnection();
  const payer = loadPayer(); // EA8Ty — the new authority + fee payer
  const members = loadOrCreateSigners(5);
  const r = readRegistry();
  const multisigPda = new PublicKey(r.multisigPda);
  const vaultPda = new PublicKey(r.vaultPda);

  const before = await readUpgradeAuthority(connection, PRESALE);
  console.log(`presale authority before: ${before?.toBase58() ?? "none"}`);
  if (before && before.equals(payer.publicKey)) {
    console.log("already on EA8Ty — nothing to do");
    return;
  }
  if (!before || !before.equals(vaultPda)) {
    throw new Error(`presale authority is ${before?.toBase58() ?? "none"}, expected the vault ${vaultPda.toBase58()}`);
  }

  const ix = setUpgradeAuthorityIx(PRESALE, vaultPda, payer.publicKey);
  const sig = await executeThroughVault({
    connection,
    multisigPda,
    payer,
    members,
    threshold: r.threshold,
    instructions: [ix],
    memo: "release presale upgrade authority to EA8Ty for extend+upgrade",
  });
  const after = await readUpgradeAuthority(connection, PRESALE);
  console.log(`presale authority after: ${after?.toBase58() ?? "none"} (${sig})`);
  if (!after || !after.equals(payer.publicKey)) {
    throw new Error("authority did not move to EA8Ty");
  }
  console.log("✓ presale authority is EA8Ty — do extend+upgrade, then hand back to the vault");
}

main().then(() => process.exit(0), (e) => { console.error(e); process.exit(1); });
