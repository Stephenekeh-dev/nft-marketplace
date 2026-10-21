import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { NftMarketplace } from "../target/types/nft_marketplace";
import {
  createMint,
  createAssociatedTokenAccount,
  mintTo,
  setAuthority,
  getAccount,
  AuthorityType,
} from "@solana/spl-token";
import { assert } from "chai";

describe("nft-marketplace", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.NftMarketplace as Program<NftMarketplace>;
  const seller = provider.wallet as anchor.Wallet;
  const connection = provider.connection;
  const TOKEN_PROGRAM_ID = anchor.utils.token.TOKEN_PROGRAM_ID;

  const treasury = anchor.web3.Keypair.generate();
  const FEE_BPS = 250; // 2.5%
  const PRICE = 1_000_000_000; // 1 SOL
  const EXPECTED_FEE = 25_000_000;

  const [marketplace] = anchor.web3.PublicKey.findProgramAddressSync(
    [Buffer.from("marketplace")],
    program.programId
  );

  async function fund(kp: anchor.web3.Keypair, sol = 5) {
    const sig = await connection.requestAirdrop(
      kp.publicKey,
      sol * anchor.web3.LAMPORTS_PER_SOL
    );
    await connection.confirmTransaction(sig, "confirmed");
  }

  /** Mints a real NFT: 0 decimals, supply 1, mint authority revoked. */
  async function createNft() {
    const mint = await createMint(connection, seller.payer, seller.publicKey, null, 0);
    const sellerTokenAccount = await createAssociatedTokenAccount(
      connection,
      seller.payer,
      mint,
      seller.publicKey
    );
    await mintTo(connection, seller.payer, mint, sellerTokenAccount, seller.payer, 1);
    await setAuthority(
      connection,
      seller.payer,
      mint,
      seller.publicKey,
      AuthorityType.MintTokens,
      null
    );
    return { mint, sellerTokenAccount };
  }

  const addressesFor = (mint: anchor.web3.PublicKey) => {
    const [listing] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("listing"), mint.toBuffer()],
      program.programId
    );
    const escrowTokenAccount = anchor.utils.token.associatedAddress({
      mint,
      owner: listing,
    });
    return { listing, escrowTokenAccount };
  };

  const listNft = (
    mint: anchor.web3.PublicKey,
    sellerTokenAccount: anchor.web3.PublicKey,
    price = PRICE
  ) => {
    const { listing, escrowTokenAccount } = addressesFor(mint);
    return program.methods
      .listNft(new anchor.BN(price))
      .accounts({
        seller: seller.publicKey,
        mint,
        sellerTokenAccount,
        listing,
        escrowTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        associatedTokenProgram: anchor.utils.token.ASSOCIATED_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .rpc();
  };

  async function expectError(promise: Promise<unknown>, expected: string) {
    try {
      await promise;
    } catch (err: any) {
      assert.include(err.toString(), expected);
      return;
    }
    assert.fail(`Expected failure containing "${expected}" but the call succeeded`);
  }

  const tokenBalance = async (address: anchor.web3.PublicKey) =>
    (await getAccount(connection, address)).amount.toString();

  it("rejects a marketplace fee above the maximum", async () => {
    await expectError(
      program.methods
        .initializeMarketplace(5000)
        .accounts({
          admin: seller.publicKey,
          treasury: treasury.publicKey,
          marketplace,
          systemProgram: anchor.web3.SystemProgram.programId,
        })
        .rpc(),
      "InvalidFee"
    );
  });

  it("initializes the marketplace", async () => {
    await program.methods
      .initializeMarketplace(FEE_BPS)
      .accounts({
        admin: seller.publicKey,
        treasury: treasury.publicKey,
        marketplace,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .rpc();

    const state = await program.account.marketplace.fetch(marketplace);
    assert.equal(state.feeBps, FEE_BPS);
    assert.equal(state.treasury.toBase58(), treasury.publicKey.toBase58());
  });

  it("lists an NFT into escrow", async () => {
    const { mint, sellerTokenAccount } = await createNft();
    const { listing, escrowTokenAccount } = addressesFor(mint);

    await listNft(mint, sellerTokenAccount);

    assert.equal(await tokenBalance(escrowTokenAccount), "1");
    assert.equal(await tokenBalance(sellerTokenAccount), "0");

    const state = await program.account.listing.fetch(listing);
    assert.equal(state.price.toString(), PRICE.toString());
    assert.equal(state.seller.toBase58(), seller.publicKey.toBase58());
  });

  it("rejects listing a token that is not an NFT", async () => {
    const mint = await createMint(connection, seller.payer, seller.publicKey, null, 6);
    const sellerTokenAccount = await createAssociatedTokenAccount(
      connection,
      seller.payer,
      mint,
      seller.publicKey
    );
    await mintTo(connection, seller.payer, mint, sellerTokenAccount, seller.payer, 5_000_000);

    await expectError(listNft(mint, sellerTokenAccount), "NotAnNft");
  });

  it("sells an NFT: pays the seller, the treasury, and moves the NFT", async () => {
    const { mint, sellerTokenAccount } = await createNft();
    const { listing, escrowTokenAccount } = addressesFor(mint);
    await listNft(mint, sellerTokenAccount);

    const buyer = anchor.web3.Keypair.generate();
    await fund(buyer);
    const buyerTokenAccount = await createAssociatedTokenAccount(
      connection,
      seller.payer,
      mint,
      buyer.publicKey
    );

    const sellerBefore = await connection.getBalance(seller.publicKey);
    const treasuryBefore = await connection.getBalance(treasury.publicKey);

    await program.methods
      .buyNft()
      .accounts({
        buyer: buyer.publicKey,
        seller: seller.publicKey,
        marketplace,
        treasury: treasury.publicKey,
        mint,
        listing,
        escrowTokenAccount,
        buyerTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([buyer])
      .rpc();

    const sellerAfter = await connection.getBalance(seller.publicKey);
    const treasuryAfter = await connection.getBalance(treasury.publicKey);

    assert.equal(await tokenBalance(buyerTokenAccount), "1");
    assert.equal(treasuryAfter - treasuryBefore, EXPECTED_FEE);
    // Seller gets the price minus the fee, plus rent refunded from the closed accounts.
    assert.isAtLeast(sellerAfter - sellerBefore, PRICE - EXPECTED_FEE);

    assert.isNull(await connection.getAccountInfo(listing), "listing should be closed");
    assert.isNull(await connection.getAccountInfo(escrowTokenAccount), "escrow should be closed");
  });

  it("lets the seller cancel and get the NFT back", async () => {
    const { mint, sellerTokenAccount } = await createNft();
    const { listing, escrowTokenAccount } = addressesFor(mint);
    await listNft(mint, sellerTokenAccount);

    await program.methods
      .cancelListing()
      .accounts({
        seller: seller.publicKey,
        mint,
        listing,
        escrowTokenAccount,
        sellerTokenAccount,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();

    assert.equal(await tokenBalance(sellerTokenAccount), "1");
    assert.isNull(await connection.getAccountInfo(listing));
    assert.isNull(await connection.getAccountInfo(escrowTokenAccount));
  });

  it("stops anyone but the seller from cancelling a listing", async () => {
    const { mint, sellerTokenAccount } = await createNft();
    const { listing, escrowTokenAccount } = addressesFor(mint);
    await listNft(mint, sellerTokenAccount);

    const attacker = anchor.web3.Keypair.generate();
    await fund(attacker, 1);
    const attackerTokenAccount = await createAssociatedTokenAccount(
      connection,
      seller.payer,
      mint,
      attacker.publicKey
    );

    let threw = false;
    try {
      await program.methods
        .cancelListing()
        .accounts({
          seller: attacker.publicKey,
          mint,
          listing,
          escrowTokenAccount,
          sellerTokenAccount: attackerTokenAccount,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([attacker])
        .rpc();
    } catch {
      threw = true;
    }

    assert.isTrue(threw, "attacker cancellation should have been rejected");
    assert.equal(await tokenBalance(escrowTokenAccount), "1");
    assert.equal(await tokenBalance(attackerTokenAccount), "0");
  });
});
