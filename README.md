# 🛒 NFT Marketplace: Solana Escrow Program

> A trustless NFT marketplace built with Anchor. Sellers list an NFT at a SOL price, buyers purchase it in a single atomic transaction, and a program-owned escrow guarantees that nobody, including the marketplace, can run off with the NFT or the money.

![Solana](https://img.shields.io/badge/Solana-Devnet-9945FF?style=flat-square&logo=solana)
![Anchor](https://img.shields.io/badge/Anchor-1.x-blue?style=flat-square)
![Rust](https://img.shields.io/badge/Rust-on--chain-DEA584?style=flat-square&logo=rust)
![Tests](https://img.shields.io/badge/tests-7%2F7%20passing-brightgreen?style=flat-square)
![License](https://img.shields.io/badge/license-MIT-lightgrey?style=flat-square)

---

## ✨ What it does

| Action | Who | What happens |
|---|---|---|
| **List** | Seller | NFT moves into an escrow token account owned by a program-derived address. A price in lamports is recorded. |
| **Buy** | Buyer | In one transaction: SOL goes to the seller, the fee goes to the treasury, and the escrow PDA signs the NFT over to the buyer. |
| **Cancel** | Seller | The escrow PDA returns the NFT and the listing is closed. Rent is refunded. |

If any part of a sale fails, the whole transaction reverts. There is no state where a buyer has paid but not received the NFT, or the reverse.

## 🏗️ System architecture

```mermaid
flowchart LR
    S(["👤 Seller wallet"])
    B(["👤 Buyer wallet"])
    T(["🏦 Treasury wallet"])

    subgraph Program["nft_marketplace program (on-chain)"]
        direction TB
        M["Marketplace PDA<br/>admin, treasury, fee_bps"]
        L["Listing PDA<br/>seller, mint, price"]
        E["Escrow token account<br/>owned by Listing PDA"]
        L --- E
    end

    TOK["SPL Token program"]

    S -- "list_nft" --> L
    S -- "cancel_listing" --> L
    B -- "buy_nft" --> L
    M -. "fee settings" .-> L
    E == "NFT" ==> B
    B -- "SOL minus fee" --> S
    B -- "fee" --> T
    Program -- "CPI transfers" --> TOK
```

The program holds no private keys. Custody is enforced entirely by PDA signing.

## 🧩 Account model

```mermaid
classDiagram
    class Marketplace {
        +Pubkey admin
        +Pubkey treasury
        +u16 fee_bps
        +u8 bump
    }
    class Listing {
        +Pubkey seller
        +Pubkey mint
        +u64 price
        +u8 bump
    }
    class EscrowTokenAccount {
        +Pubkey mint
        +Pubkey owner = Listing PDA
        +u64 amount = 1
    }
    Marketplace <.. Listing : fee read at purchase
    Listing "1" --> "1" EscrowTokenAccount : authority of
```

### PDA derivation

| Account | Seeds | Notes |
|---|---|---|
| Marketplace | `["marketplace"]` | One per program deployment |
| Listing | `["listing", nft_mint]` | One active listing per NFT |
| Escrow token account | Associated token address of `(nft_mint, Listing PDA)` | Created at list time, closed on sale or cancel |

## 🔄 Instruction flows

### List

```mermaid
sequenceDiagram
    autonumber
    actor Seller
    participant P as Marketplace program
    participant T as SPL Token program
    Seller->>P: list_nft(price)
    P->>P: validate mint is an NFT
    P->>P: create Listing PDA and escrow account
    P->>T: transfer 1 NFT, seller to escrow
    P-->>Seller: emit Listed event
```

### Buy

```mermaid
sequenceDiagram
    autonumber
    actor Buyer
    participant P as Marketplace program
    participant Y as System program
    participant T as SPL Token program
    Buyer->>P: buy_nft()
    P->>P: compute fee, seller_amount = price - fee
    P->>Y: transfer seller_amount, buyer to seller
    P->>Y: transfer fee, buyer to treasury
    P->>T: Listing PDA signs, NFT to buyer
    P->>T: close escrow account, rent to seller
    P->>P: close Listing, rent to seller
    P-->>Buyer: emit Sold event
```

### Cancel

```mermaid
sequenceDiagram
    autonumber
    actor Seller
    participant P as Marketplace program
    participant T as SPL Token program
    Seller->>P: cancel_listing()
    P->>P: require signer is the listing's seller
    P->>T: Listing PDA signs, NFT back to seller
    P->>T: close escrow account
    P->>P: close Listing
    P-->>Seller: emit Cancelled event
```

### Listing lifecycle

```mermaid
stateDiagram-v2
    [*] --> Listed: list_nft
    Listed --> Sold: buy_nft
    Listed --> Cancelled: cancel_listing
    Sold --> [*]
    Cancelled --> [*]
```

## 💰 Fee model

```
fee           = price × fee_bps ÷ 10,000
seller_amount = price − fee
```

Example at the default 250 bps (2.5%) for a 1 SOL listing:

| Recipient | Amount |
|---|---|
| Seller | 0.975 SOL |
| Treasury | 0.025 SOL |
| Buyer pays | 1.000 SOL |

Fee maths runs in `u128` with checked operations, and the fee is capped at **1,000 bps (10%)** at initialization.

## 📚 Instruction reference

| Instruction | Arguments | Signer | Accounts touched |
|---|---|---|---|
| `initialize_marketplace` | `fee_bps: u16` | Admin | marketplace (init), treasury |
| `list_nft` | `price: u64` | Seller | mint, seller token account, listing (init), escrow (init) |
| `buy_nft` | none | Buyer | marketplace, treasury, seller, mint, listing (close), escrow (close), buyer token account |
| `cancel_listing` | none | Seller | mint, listing (close), escrow (close), seller token account |

### Events

| Event | Fields |
|---|---|
| `Listed` | seller, mint, price |
| `Sold` | seller, buyer, mint, price, fee |
| `Cancelled` | seller, mint |

### Errors

| Error | Raised when |
|---|---|
| `InvalidFee` | Fee above 10% |
| `InvalidPrice` | Price is zero |
| `NotAnNft` | Mint doesn't have 0 decimals and a supply of 1 |
| `MintAuthorityNotRevoked` | The mint could still create more tokens |
| `NftNotOwned` | Seller's token account doesn't hold the NFT |
| `MathOverflow` | A checked arithmetic operation failed |

## 🛡️ Security design

| Threat | Mitigation |
|---|---|
| Listing a fungible token as an "NFT" | Requires `decimals == 0` and `supply == 1` |
| Listing an NFT whose supply could be inflated | Requires the mint authority to be revoked |
| A stranger cancelling someone's listing | `Signer` plus `has_one = seller` on the listing |
| Fees redirected to another wallet | `has_one = treasury` on the marketplace account |
| Payment sent to a different seller | `has_one = seller` on the listing |
| Buyer token account for the wrong mint or owner | `token::mint` and `token::authority` constraints |
| Fee overflow or underflow | `u128` intermediate maths, checked operations, 10% cap |
| Replaying a sale | Listing and escrow are closed in the same transaction |
| Stranded rent | Closed accounts refund lamports to the seller |

### Known limitations

- **SOL payments only.** SPL-token pricing is a planned extension.
- **No royalties yet.** Creator royalties need Metaplex metadata parsing (see roadmap).
- **No price updates.** To change a price, cancel and relist.
- **Fixed fee.** There is no `update_fee` instruction yet.
- **Buyer token account must exist.** The client creates it in the same transaction.
- **Not audited.** This is a portfolio project. Don't use it with real funds.

## 🧪 Test suite

Seven integration tests run against a live local validator (Surfpool), using real SPL token mints rather than mocks.

| Test | Verifies |
|---|---|
| Rejects a fee above the maximum | Initialization guard |
| Initializes the marketplace | State is stored correctly |
| Lists an NFT into escrow | NFT moves, price and seller recorded |
| Rejects a non-NFT token | `NotAnNft` for a 6-decimal token |
| Sells an NFT | Exact fee to treasury, seller paid, NFT to buyer, accounts closed |
| Seller can cancel | NFT returned, listing and escrow closed |
| Non-seller cannot cancel | Attacker rejected, escrow untouched |

## 🚀 Getting started

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install)
- [Solana CLI](https://docs.solana.com/cli/install-solana-cli-tools)
- [Anchor CLI](https://www.anchor-lang.com/docs/installation) 1.x
- [Surfpool](https://docs.surfpool.run/): `curl -sL https://run.surfpool.run/ | bash`
- Node.js 18+

### Build and test

```bash
git clone <your-repo-url>
cd nft-marketplace
npm install
anchor build
anchor test
```

### Deploy to devnet

```bash
solana config set --url https://api.devnet.solana.com
anchor deploy --provider.cluster devnet
node init-marketplace.js     # one-time: creates the Marketplace PDA
```

> **Known toolchain issue:** on Anchor 1.1.x the deploy may end with `Failed to initialize IDL`. The program upgrade itself still succeeds (check with `solana program show`), and the client uses the local IDL file, so this is safe to ignore.

### 📍 Deployment

| Network | Program ID |
|---|---|
| Devnet | `E6CZ4yrxREvTUUjHz2hD4vAATW2yhKoFUTi1MKrXXp9F` |

## 📁 Project structure

```
nft-marketplace/
├── programs/
│   └── nft-marketplace/
│       └── src/lib.rs          # instructions, accounts, events, errors
├── tests/
│   └── nft-marketplace.ts      # integration tests
├── init-marketplace.js         # one-time devnet setup script
├── run-tests.js                # Mocha runner (ts-node, CommonJS)
├── Anchor.toml
└── Cargo.toml
```

## 🧰 Toolchain notes

Lessons from building on Anchor 1.x, in case they save you a day:

- The TypeScript SDK is now `@anchor-lang/core`, renamed from `@coral-xyz/anchor`.
- `CpiContext::new` takes the program's `Pubkey` (`.key()`), not its `AccountInfo`.
- `anchor-spl` needs its `idl-build` feature enabled alongside `anchor-lang`'s.
- `anchor test` defaults to Surfpool, which must be installed separately.
- Pinning `uuid` to 9.0.1 avoids a CommonJS/ESM crash in `rpc-websockets`.
- A small custom Mocha runner avoids ESM loader problems with `.ts` test files on Node 18.

## 🗺️ Roadmap

- [x] Escrow listing, purchase and cancel
- [x] Marketplace fee with on-chain cap
- [x] Events for indexers
- [x] Integration test suite
- [ ] Devnet deployment and marketplace initialization
- [ ] Angular frontend: browse, list, buy, cancel
- [ ] Metaplex metadata checks and creator royalties
- [ ] SPL-token pricing
- [ ] `update_price` and `update_fee` instructions
- [ ] Offers and auctions

## 📄 License

MIT

## 👤 Author

Built by Stephen as part of a hands-on Solana portfolio series, following a PDA token vault. Questions or feedback are welcome.