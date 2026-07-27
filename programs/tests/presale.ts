import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { Presale } from "../target/types/presale";

describe("presale", () => {
  // Configure the client to use the local cluster.
  anchor.setProvider(anchor.AnchorProvider.env());

  const program = anchor.workspace.presale as Program<Presale>;

  it("initializes the sale config singleton", async () => {
    const [saleConfig] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("sale_config")],
      program.programId,
    );

    const tx = await program.methods
      .initialize()
      .accountsPartial({ saleConfig })
      .rpc();
    console.log("Initialize transaction signature", tx);

    const account = await program.account.saleConfig.fetch(saleConfig);
    if (!account.admin.equals(program.provider.publicKey!)) {
      throw new Error("sale_config.admin does not match the payer");
    }
  });
});
