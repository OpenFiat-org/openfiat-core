/**
 * Switch on arbitrator sortition on devnet.
 *
 * The sortition CODE has been live in the deployed escrow program since
 * #118, but `FeeConfig.arbitrator_sortition_bps` is 0, and zero disables
 * the draw entirely — `qualifies_for_seat` returns true for everyone. So
 * the 500 OPEN arbitrator floor was protected by a mechanism that was not
 * switched on.
 *
 * `update_fee_config` restates every parameter, so this reads the live
 * account first and writes back what is there, changing only the two
 * fields it means to change. Decoding uses a single moving cursor and
 * asserts the walk consumes the whole account, for the reason recorded
 * against StakingConfig: per-field offsets keep computing after a layout
 * shift and read every later value out of the wrong place.
 */
import {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { readFileSync } from "node:fs";

const ESCROW = new PublicKey("HaPpM1QYM3dKp3sX7zhEdft9hB6ncu6xfALAbkyQChQP");
const FEE_CONFIG = new PublicKey("6JPUB6RUxgpDPWZqcXhkMYXV8gofMYxwbMfNr8fLAUHX");
const MAX_SETTLEMENT_MINTS = 16;

interface FeeConfig {
  admin: PublicKey;
  adListingFee: bigint;
  disputeFilingFee: bigint;
  settlementFeeBps: number;
  devTreasury: PublicKey;
  ecosystemTreasury: PublicKey;
  infraTreasury: PublicKey;
  emergencyReserve: PublicKey;
  devTreasuryBps: number;
  ecosystemTreasuryBps: number;
  infraTreasuryBps: number;
  emergencyReserveBps: number;
  timeoutSecs: bigint;
  bump: number;
  minArbitratorStakeAgeSecs: bigint;
  arbitratorSortitionBps: number;
  settlementMints: PublicKey[];
  settlementMintCount: number;
}

function decode(data: Buffer): FeeConfig {
  let at = 8; // discriminator
  const key = () => { const k = new PublicKey(data.subarray(at, at + 32)); at += 32; return k; };
  const u64 = () => { const v = data.readBigUInt64LE(at); at += 8; return v; };
  const i64 = () => { const v = data.readBigInt64LE(at); at += 8; return v; };
  const u16 = () => { const v = data.readUInt16LE(at); at += 2; return v; };
  const u8 = () => { const v = data.readUInt8(at); at += 1; return v; };

  const config: FeeConfig = {
    admin: key(),
    adListingFee: u64(),
    disputeFilingFee: u64(),
    settlementFeeBps: u16(),
    devTreasury: key(),
    ecosystemTreasury: key(),
    infraTreasury: key(),
    emergencyReserve: key(),
    devTreasuryBps: u16(),
    ecosystemTreasuryBps: u16(),
    infraTreasuryBps: u16(),
    emergencyReserveBps: u16(),
    timeoutSecs: i64(),
    bump: u8(),
    minArbitratorStakeAgeSecs: i64(),
    arbitratorSortitionBps: u16(),
    settlementMints: Array.from({ length: MAX_SETTLEMENT_MINTS }, () => key()),
    settlementMintCount: u8(),
  };

  if (at !== data.length) {
    throw new Error(
      `decode consumed ${at} of ${data.length} bytes — the layout this script ` +
        `assumes is not the layout on chain, so every value read is suspect`,
    );
  }
  return config;
}

function encodeUpdate(next: FeeConfig): Buffer {
  // Discriminator for `update_fee_config`, from the built IDL.
  const idl = JSON.parse(readFileSync("target/idl/escrow.json", "utf8"));
  const ix = idl.instructions.find((i: { name: string }) => i.name === "update_fee_config");
  if (!ix) throw new Error("update_fee_config is not in the IDL");

  const parts: Buffer[] = [Buffer.from(ix.discriminator)];
  const u64 = (v: bigint) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(v); return b; };
  const i64 = (v: bigint) => { const b = Buffer.alloc(8); b.writeBigInt64LE(v); return b; };
  const u16 = (v: number) => { const b = Buffer.alloc(2); b.writeUInt16LE(v); return b; };

  parts.push(
    u64(next.adListingFee),
    u64(next.disputeFilingFee),
    u16(next.settlementFeeBps),
    u16(next.devTreasuryBps),
    u16(next.ecosystemTreasuryBps),
    u16(next.infraTreasuryBps),
    u16(next.emergencyReserveBps),
    i64(next.timeoutSecs),
    i64(next.minArbitratorStakeAgeSecs),
    u16(next.arbitratorSortitionBps),
  );

  // `settlement_mints: Vec<Pubkey>` — Borsh writes a u32 length then the
  // elements. Restated from the live account: this instruction replaces
  // the allowlist wholesale, so omitting it would empty it and stop every
  // new escrow rather than changing a sortition parameter.
  const active = next.settlementMints.slice(0, next.settlementMintCount);
  const length = Buffer.alloc(4);
  length.writeUInt32LE(active.length);
  parts.push(length, ...active.map((mint) => Buffer.from(mint.toBytes())));

  return Buffer.concat(parts);
}

async function main() {
  const conn = new Connection(process.env.RPC!, "confirmed");
  const admin = Keypair.fromSecretKey(
    Uint8Array.from(JSON.parse(readFileSync(process.env.ADMIN_KEYPAIR!, "utf8"))),
  );

  const before = decode((await conn.getAccountInfo(FEE_CONFIG))!.data);
  console.log("BEFORE");
  console.log("  arbitrator_sortition_bps      =", before.arbitratorSortitionBps);
  console.log("  min_arbitrator_stake_age_secs =", before.minArbitratorStakeAgeSecs.toString());
  console.log("  settlement_fee_bps            =", before.settlementFeeBps);
  console.log("  splits (dev/eco/infra/emerg)  =",
    [before.devTreasuryBps, before.ecosystemTreasuryBps, before.infraTreasuryBps, before.emergencyReserveBps].join("/"));
  console.log("  timeout_secs                  =", before.timeoutSecs.toString());
  console.log("  admin                         =", before.admin.toBase58());
  console.log("  settlement_mint_count         =", before.settlementMintCount);

  if (before.admin.toBase58() !== admin.publicKey.toBase58()) {
    throw new Error(`this keypair is not the config admin (${before.admin.toBase58()})`);
  }
  if (process.env.DRY_RUN === "1") {
    console.log("\nDRY_RUN=1 — nothing sent.");
    return;
  }

  const next: FeeConfig = {
    ...before,
    arbitratorSortitionBps: Number(process.env.SORTITION_BPS ?? 100),
    minArbitratorStakeAgeSecs: BigInt(process.env.STAKE_AGE_SECS ?? "3600"),
  };

  // The four treasuries are re-validated by the program as token accounts
  // of `mint`, which is the point: this instruction cannot store a wallet
  // where a token account belongs. They are passed from the DECODED
  // account rather than from a constant, so what is re-validated is what
  // is actually stored.
  const settlementMint = new PublicKey(
    process.env.SETTLEMENT_MINT ?? "SK1JEbfsjjTG2WELNirmM7iJVcdnwerqfF32kCnoWsM",
  );
  const ix = new TransactionInstruction({
    programId: ESCROW,
    keys: [
      { pubkey: admin.publicKey, isSigner: true, isWritable: false },
      { pubkey: FEE_CONFIG, isSigner: false, isWritable: true },
      { pubkey: settlementMint, isSigner: false, isWritable: false },
      { pubkey: before.devTreasury, isSigner: false, isWritable: false },
      { pubkey: before.ecosystemTreasury, isSigner: false, isWritable: false },
      { pubkey: before.infraTreasury, isSigner: false, isWritable: false },
      { pubkey: before.emergencyReserve, isSigner: false, isWritable: false },
    ],
    data: encodeUpdate(next),
  });

  const signature = await conn.sendTransaction(new Transaction().add(ix), [admin]);
  await conn.confirmTransaction(signature, "confirmed");
  console.log("\nupdate_fee_config:", signature);

  const after = decode((await conn.getAccountInfo(FEE_CONFIG))!.data);
  console.log("AFTER");
  console.log("  arbitrator_sortition_bps      =", after.arbitratorSortitionBps);
  console.log("  min_arbitrator_stake_age_secs =", after.minArbitratorStakeAgeSecs.toString());

  // Everything else must be untouched. A parameter write nobody audits is
  // exactly how the treasury and slash_destination defects survived.
  const unchanged: (keyof FeeConfig)[] = [
    "admin", "adListingFee", "disputeFilingFee", "settlementFeeBps",
    "devTreasury", "ecosystemTreasury", "infraTreasury", "emergencyReserve",
    "devTreasuryBps", "ecosystemTreasuryBps", "infraTreasuryBps",
    "emergencyReserveBps", "timeoutSecs", "bump", "settlementMintCount",
  ];
  for (const field of unchanged) {
    const a = String(before[field]);
    const b = String(after[field]);
    if (a !== b) throw new Error(`${field} moved: ${a} -> ${b}`);
  }
  for (let i = 0; i < MAX_SETTLEMENT_MINTS; i++) {
    if (!before.settlementMints[i].equals(after.settlementMints[i])) {
      throw new Error(`settlement_mints[${i}] moved`);
    }
  }
  console.log("  every other field verified identical before and after");
}

main().catch((e) => { console.error(e); process.exit(1); });
