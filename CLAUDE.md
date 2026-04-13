# CLAUDE.md — Project Context for CURS3D

## What is this project?

CURS3D is a quantum-resistant Layer 1 blockchain written in Rust from scratch. It is NOT a fork of any existing blockchain. Every component — consensus, crypto, networking, storage, API — is implemented from zero.

## Build & Test

```bash
cargo build --release    # Build
cargo test               # 46 tests
cargo clippy             # Lint (must pass with no warnings in CI)
cargo fmt --check        # Format check
```

Rust edition: 2024. Minimum toolchain: stable 1.94+.

## Project Structure

```
src/
  api/mod.rs           HTTP REST API (hyper 1.x, port 8080)
  consensus/mod.rs     BFT PoS, FinalityVote, FinalityTracker, EquivocationEvidence, slashing
  core/
    block.rs           BlockHeader, Block, genesis, signatures, verification
    blocktree.rs       BlockTree, fork choice (heaviest chain), pruning
    chain.rs           Blockchain struct, state management, validation, reorg
    transaction.rs     Transaction, TransactionKind (Transfer, Stake, Unstake, Coinbase)
    mod.rs
  crypto/
    dilithium.rs       CRYSTALS-Dilithium Level 5 (pqcrypto crate)
    hash.rs            SHA-3, double_hash, merkle_root, address derivation
    mod.rs
  network/mod.rs       libp2p P2P, Gossipsub, mDNS, sync, block production
  rpc/mod.rs           TCP JSON RPC (port 9545, used by CLI)
  storage/mod.rs       sled database (blocks, accounts, pending, evidence, meta)
  wallet/mod.rs        Wallet with AES-256-GCM + Argon2 encryption
  lib.rs               Module declarations
  main.rs              CLI entry point (clap)

website/               Static documentation site (7 HTML pages + CSS + JS)
```

## Key Types and Constants

### core/chain.rs
- `Blockchain` — Main state holder (blocks, accounts, pending_txs, block_tree, finality_tracker, slashed_validators, storage)
- `AccountState` — {balance, nonce, staked_balance, public_key}
- `GenesisConfig` — {chain_name, block_reward, minimum_stake, allocations}
- `ChainError` — All validation errors
- `DEFAULT_BLOCK_REWARD = 50_000_000` (50 CUR in microtokens)
- `DEFAULT_MIN_STAKE = 1_000_000_000` (1000 CUR)
- Token unit: 1 CUR = 1_000_000 microtokens

### core/transaction.rs
- `TransactionKind` — Transfer, Stake, Unstake, Coinbase
- Transactions include `sender_public_key` for signature verification
- `from` address is derived: SHA-3(public_key)[0..20]

### core/blocktree.rs
- `BlockTree` — Stores all known blocks including forks
- Fork choice: heaviest cumulative proposer-stake wins
- `set_finalized()` triggers pruning of non-canonical branches

### consensus/mod.rs
- `ProofOfStake` — Validator selection, slashing
- `FinalityVote` — Signed attestation for a block
- `FinalityTracker` — Accumulates votes, triggers finality at 2/3 threshold
- `EquivocationEvidence` — Two different block signatures at same height from same validator
- Slashing penalty: 33% of staked balance

### crypto/hash.rs
- `ADDRESS_LEN = 20` bytes
- Address format: `CUR` + 40 hex chars (human-readable) or raw 20 bytes (internal)

### network/mod.rs
- NetworkMessage variants: NewBlock, NewTransaction, RequestBlocks, BlockResponse, HeightAnnounce, SlashingEvidence, FinalityVote
- Block production: every 10 seconds
- Height announce: every 30 seconds
- Sync: batch of 50 blocks, 15s timeout, 3 retries

### api/mod.rs
- HTTP server on port 8080
- All responses: `{"ok": true, "data": {...}}` or `{"ok": false, "error": "..."}`
- CORS enabled (Access-Control-Allow-Origin: *)
- Faucet gives 100 CUR (testnet only)

## Coding Patterns

### Adding a new transaction type
1. Add variant to `TransactionKind` in `core/transaction.rs`
2. Add constructor method on `Transaction`
3. Add shape validation in `chain.rs::validate_transaction_shape()`
4. Add application logic in `chain.rs::apply_user_transaction()`
5. Add test

### Adding a new API endpoint
1. Add match arm in `api/mod.rs::handle_request()`
2. Create response struct with `#[derive(Serialize)]`
3. Return `json_ok(data)` or `json_err(status, msg)`

### Adding a new network message
1. Add variant to `NetworkMessage` in `network/mod.rs`
2. Handle in `run_with_chain()` match on network events
3. Broadcast with `self.broadcast(&msg)`

## Important Conventions

- Addresses are 20 bytes internally, displayed as `CUR` + 40 hex chars
- All amounts are in **microtokens** (1 CUR = 1_000_000)
- Block hashes use double-SHA3: `sha3(sha3(bincode(header)))`
- Genesis block has height 0, no signature, fixed timestamp 1_700_000_000
- The TCP RPC (port 9545) is for CLI. The HTTP API (port 8080) is for browsers/apps
- Wallet files are encrypted with AES-256-GCM. Argon2 derives the key from password
- `load_auto()` auto-migrates old plaintext wallets to encrypted format

## Tests

46 tests across modules:
- consensus: 8 (validators, selection, slashing, equivocation, finality votes, dedup)
- core/block: 2 (genesis, new block)
- core/blocktree: 5 (basic, fork choice, common ancestor, reject below finalized, pruning)
- core/chain: 8 (new, genesis config, create block, tx flow, forged mint, stake, unstake, duplicate, state root)
- core/transaction: 5 (sign/verify, coinbase, stake, unstake, forged from)
- crypto: 5 (sha3, merkle, address, sign/verify, invalid sig)
- storage: 5 (block, account, height, pending, meta)
- wallet: 5 (create, deterministic address, encrypted save/load, wrong password, auto-migrate)

Run a specific test: `cargo test test_name --lib`

## Dependencies (key ones)

- `pqcrypto-dilithium` — Post-quantum signatures
- `sha3` — Keccak hashing
- `sled` — Embedded key-value database
- `libp2p` — P2P networking (Gossipsub + mDNS)
- `hyper` — HTTP server
- `aes-gcm` + `argon2` — Wallet encryption
- `clap` — CLI parsing
- `tokio` — Async runtime
- `serde` + `bincode` — Serialization

## Website

7 pages in `website/`:
- `index.html` — Landing page
- `docs.html` — Full documentation with sidebar
- `examples.html` — 9 step-by-step tutorials
- `whitepaper.html` — Technical whitepaper v2.0
- `explorer.html` — Block explorer (connects to REST API on localhost:8080)
- `style.css` — Shared dark theme
- `script.js` — Particles, scroll animations, hamburger menu

Serve locally: `cd website && python3 -m http.server 3000`
