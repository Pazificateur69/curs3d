# CLAUDE.md — Project Context for CURS3D

## What is this project?

CURS3D is a quantum-resistant Layer 1 blockchain written in Rust from scratch. It is NOT a fork of any existing blockchain. Every component — consensus, crypto, networking, storage, VM, API — is implemented from zero.

**Live Multi-Node Testnet (2 validators active, extensible to 4):**
- API: https://api.curs3d.fr/api/status
- Website: https://curs3d.fr
- Explorer: https://explorer.curs3d.fr
- WebSocket: wss://api.curs3d.fr/ws
- P2P Bootnode: 144.24.192.222:4337 (PeerId: 12D3KooWGy9BLopUe6CmnxuFgk5pCou8Kj5R1DXa9XLga9MD63va)
- Faucet: POST https://api.curs3d.fr/api/faucet/request
- Node1 (bootstrap+API): 144.24.192.222 — ssh curs3d-node1 — Validator CURe1Fa551B3f0524EfD8d0673cdBF9fD0e199458c5
- Node2 (validator): 84.235.238.213 — ssh curs3d-node2 — Validator CURdC1ecceD4f12Cb3E34BD0d43E72d6D04fC4823dd
- Faucet: CUR34cafc74B750C0e0150877e99cd27D77C6c4fC44
- Hosting: Oracle Cloud ARM Free Tier (1 OCPU / 6 GB per node), eu-marseille-1
- Runbook: deploy/DEPLOY_RUNBOOK.md
- IMPORTANT: Wallets must be created ON the server (not cross-compiled) due to Argon2id

**Security hardening (2026-04-24, internal 3-AI council audit):**
- Governance: snapshot-based voting prevents stake-transfer double-vote attacks
- P2P: bounded deserialization (16MB limit) prevents OOM from malicious payloads
- Fee market: base_fee >= 1 always (prevents free spam attacks)
- Mempool: nonce floor check rejects stale transactions
- Chain: reorg depth capped at 64 blocks
- Wallet: Argon2id hardened (m=64MB, t=3, p=4) replaces weak defaults
- Block validation: strict timestamp monotonicity
- Storage: snapshot chunk index validation prevents bloat attacks
- Zero Box::leak memory leaks in error handling

## Build & Test

```bash
cargo build --release    # Build
cargo test --lib         # 129 tests
cargo clippy --lib -- -D warnings  # Lint (must pass with zero warnings in CI)
cargo fmt --check        # Format check
```

Rust edition: 2024. CI runs on nightly (wasmer probestack workaround) with `--lib` targets.

## Project Structure

```
src/
  api/mod.rs           HTTP REST API (hyper 1.x, port 8080)
                       - 20 endpoints + WebSocket (/ws)
                       - Per-IP rate limiting (60 GET/min, 10 POST/min)
                       - Faucet: POST /api/faucet/request (100 CUR, 1h cooldown)
                       - Bearer token auth (optional via CURS3D_API_TOKEN)
                       - CORS configurable (CURS3D_API_ALLOW_ORIGIN)
                       - 128 max HTTP connections, 64 max WebSocket connections, 1MB body limit
                       - Token endpoints: /api/tokens, /api/token/:addr, /api/token/:addr/balance/:owner
                       - Governance endpoints: /api/governance/proposals, /api/governance/proposal/:id
  consensus/mod.rs     BFT PoS, FinalityVote, FinalityTracker, EquivocationEvidence, slashing, epoch rewards, inactivity penalties
  core/
    block.rs           BlockHeader, Block, genesis, signatures, verification
    blocktree.rs       BlockTree, fork choice (heaviest chain), pruning
    chain.rs           Blockchain struct, state management, validation, reorg, fee market, snapshots, token/governance dispatch
    transaction.rs     Transaction, 12 types: Transfer, Stake, Unstake, Coinbase, DeployContract, CallContract, DeployToken, TokenTransfer, TokenApprove, TokenTransferFrom, SubmitProposal, GovernanceVote
    receipt.rs         Receipt with gas details, LogEntry, IndexedReceipt, LogFilter
    state_proof.rs     AccountProof, StorageProof (Merkle inclusion)
    mod.rs
  crypto/
    dilithium.rs       CRYSTALS-Dilithium Level 5 (pqcrypto crate)
    hash.rs            SHA-3, sha3_hash_domain (domain separation), double_hash, merkle trees/proofs, checksummed addresses (EIP-55 style), address derivation
    mod.rs
  governance/mod.rs    On-chain governance: proposals, voting (stake-weighted), automatic execution
  light/mod.rs         Light client: header-only sync, Merkle proof verification
  network/mod.rs       libp2p 0.54 P2P, Gossipsub, mDNS, sync, block production, state sync, per-peer rate limiting, peer scoring/reputation
  rpc/mod.rs           TCP JSON RPC (port 9545, used by CLI)
  storage/mod.rs       sled database (10 trees). Schema v4.
  token/mod.rs         CUR-20 token standard: deploy, transfer, approve, transferFrom, registry
  trie/mod.rs          Sparse Merkle Trie: 256-bit key space, O(log n) proofs, incremental updates
  vm/
    mod.rs             Wasmer 5 WASM execution, Cranelift, fuel middleware, 11 host functions
    gas.rs             Gas cost schedule
    state.rs           ContractState (code, storage, owner)
  wallet/mod.rs        Wallet with AES-256-GCM + Argon2 encryption, auto-migration
  lib.rs               Module declarations
  main.rs              CLI entry point (clap 4, 11 commands)

website/               Documentation site (6 HTML pages + CSS + JS)
```

## Key Types and Constants

### core/chain.rs
- `Blockchain` — Main state holder (blocks, accounts, contracts, receipts, pending_txs, block_tree, finality_tracker, slashed_validators, epoch_snapshots, storage)
- `AccountState` — {balance, nonce, staked_balance, pending_unstakes, validator_active_from_height, jailed_until_height, public_key}
- `GenesisConfig` — {chain_id, chain_name, block_reward, minimum_stake, unstake_delay_blocks, epoch_length, jail_duration_blocks, allocations, upgrades, block_gas_limit, initial_base_fee_per_gas, base_fee_change_denominator}
- `TransactionEstimate` — Dry-run result with gas, fees, replacement check
- `ChainError` — All validation errors
- `DEFAULT_BLOCK_REWARD = 50_000_000` (50 CUR in microtokens)
- `DEFAULT_MIN_STAKE = 1_000_000_000` (1000 CUR)
- `DEFAULT_BLOCK_GAS_LIMIT = 10_000_000`
- `DEFAULT_EPOCH_LENGTH = 32`
- `DEFAULT_JAIL_DURATION_BLOCKS = 64`
- `DEFAULT_UNSTAKE_DELAY_BLOCKS = 10`
- Token unit: 1 CUR = 1_000_000 microtokens

### core/transaction.rs
- `TransactionKind` — Transfer, Stake, Unstake, Coinbase, DeployContract, CallContract, DeployToken, TokenTransfer, TokenApprove, TokenTransferFrom, SubmitProposal, GovernanceVote
- Transactions include `sender_public_key` for signature verification
- `from` address is derived: SHA-3(public_key)[0..20]
- EIP-1559 fields: fee, max_fee_per_gas, max_priority_fee_per_gas, gas_limit
- Data field for contract bytecode/input

### core/blocktree.rs
- `BlockTree` — Stores all known blocks including forks
- Fork choice: heaviest cumulative proposer-stake wins
- `set_finalized()` triggers pruning of non-canonical branches

### consensus/mod.rs
- `ProofOfStake` — Validator selection, slashing
- `FinalityVote` — Signed attestation for a block (block_hash || height || epoch)
- `FinalityTracker` — Accumulates votes, triggers finality at 2/3 threshold
- `EquivocationEvidence` — Two different block headers at same height from same validator
- `EpochSnapshot` — Frozen validator set per epoch
- `EpochSettlement` — Epoch rewards + inactivity penalties (computed and applied at epoch boundaries)
- `compute_epoch_settlement()` — Rewards proportional to stake * blocks produced
- `apply_epoch_settlement()` — Distributes rewards to liquid balance, deducts penalties from staked
- Slashing penalty: 33% of staked balance + jail
- Inactivity: grace period of 2 epochs, then escalating stake penalties
- Epoch reward rate: 100 microtokens per CUR staked per block produced

### crypto/hash.rs
- `ADDRESS_LEN = 20` bytes
- Address format: `CUR` + 40 hex chars with EIP-55 style checksum (mixed case)
- `sha3_hash_domain()` — Domain-separated hashing to prevent cross-layer collisions
- `checksum_address()` / `verify_checksum_address()` — Typo-detecting addresses
- Merkle proof generation and verification

### network/mod.rs
- NetworkMessage variants: NewBlock, NewTransaction, RequestBlocks, BlockResponse, HeightAnnounce (signed), SlashingEvidence, FinalityVote, RequestSnapshot, SnapshotManifest, SnapshotChunk
- `PeerRateLimiter` — Per-peer message rate limiting with escalating bans
- `PeerScorer` — Reputation system: score decay, behavior-based scoring, automatic ban below threshold
- Block acceptance → positive score, block rejection → negative score, rate limit → penalty
- Block production: every 10 seconds
- Height announce: every 30 seconds (signed by validators)
- Sync: batch of 50 blocks, 15s timeout, 3 retries
- Network topic: derived from chain_id + protocol_version

### vm/mod.rs
- Wasmer 5 with Cranelift backend
- Instruction-level fuel metering via FuelMeteringModule middleware
- Contracts with unmetered loops rejected at deploy time
- 11 host functions: storage_get, storage_set, storage_read, storage_write_bytes, emit_log, emit_log_bytes, input, input_len, input_read, consume_gas, loop_tick
- Gas costs in gas.rs: base_tx=21000, deploy=32000, call=2600, storage_read=200, storage_write=5000, log=375, per_byte=16, loop_tick=50

### api/mod.rs
- HTTP server on port 8080
- All responses: `{"ok": true, "data": {...}}` or `{"ok": false, "error": "..."}`
- Rate limiting: 60 GET/min, 10 POST/min per IP
- Faucet: 100 CUR, 1 hour cooldown per address
- Auth: optional via CURS3D_API_TOKEN env var
- CORS: configurable via CURS3D_API_ALLOW_ORIGIN env var

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
- EIP-1559: base_fee adjusts toward 50% gas target per block
- Epochs freeze validator sets for deterministic selection

## Tests

129 tests across 15 modules:
- consensus: 15 (validators, selection, slashing, equivocation, finality votes, dedup, jailing, epochs, epoch rewards, inactivity penalty, grace period, apply settlement)
- core/block: 2 (genesis, new block)
- core/blocktree: 6 (basic, fork choice, common ancestor, reject below finalized, pruning, branch rejection)
- core/chain: 28 (genesis, config, blocks, tx flow, forged mint, stake, unstake, duplicate, state root, contracts, receipts, snapshots, fee market, epochs, state proofs, restart)
- core/transaction: 5 (sign/verify, coinbase, stake, unstake, forged from)
- crypto/dilithium: 2 (sign/verify, invalid sig)
- crypto/hash: 7 (sha3, merkle root, merkle proof, address derivation, domain separation, checksum roundtrip, checksum rejection)
- governance: 8 (submit, vote, double vote, pass/execute, reject no quorum, reject no approval, invalid param, vote after deadline)
- light: 3 (new client, valid proof, invalid proof, empty headers)
- network: 9 (rate limiter: normal traffic, flood block, peer isolation, cleanup, escalating bans; peer scoring: good behavior, bad->ban, decay, clamped)
- storage: 7 (block, account, height, pending, meta, epochs, snapshots)
- token: 10 (deploy, transfer, insufficient balance, approve+transferFrom, insufficient allowance, duplicate deploy, invalid params, zero amount, self transfer, list)
- trie: 9 (empty, insert/get, root changes, deterministic root, remove restores, proof generation, proof absent, many entries, update value)
- vm: 10 (deploy valid/invalid/empty/oom, call, storage+logs, deterministic address, unmetered loop, instruction metering)
- wallet: 5 (create, deterministic address, encrypted save/load, wrong password, auto-migrate)

Run a specific test: `cargo test test_name --lib`

## Dependencies (key ones)

- `pqcrypto-dilithium` — Post-quantum signatures (Dilithium Level 5)
- `sha3` — Keccak hashing
- `sled` — Embedded key-value database
- `libp2p` 0.54 — P2P networking (Gossipsub + mDNS + noise + yamux)
- `hyper` 1.x — HTTP server
- `wasmer` 5 + `wasmer-types` 5 — WASM VM with Cranelift
- `aes-gcm` + `argon2` — Wallet encryption
- `clap` 4 — CLI parsing
- `tokio` — Async runtime
- `serde` + `bincode` — Serialization
- `chrono` — Timestamps
- `thiserror` — Error types
- `tracing` — Logging

## Environment Variables

- `CURS3D_API_TOKEN` — Bearer token for POST endpoint auth
- `CURS3D_RPC_TOKEN` — Token for TCP RPC auth
- `CURS3D_API_ALLOW_ORIGIN` — CORS allowed origin (blocked if unset)

## Website

**Live:** https://explorer.curs3d.fr

10 pages in `website/`, bilingual EN/FR:
- `index.html` — Landing page with live stats (4 validators, 129 tests, 12 tx types, 22 endpoints)
- `docs.html` — Full documentation: 11 CLI commands, 22 API endpoints + WebSocket, multi-node operations
- `examples.html` — 10 step-by-step examples (node, multi-node connect, faucet, tokens, WebSocket, SDKs)
- `explorer.html` — Block explorer (connects to https://api.curs3d.fr by default, WebSocket live feed)
- `whitepaper.html` — Technical whitepaper
- `governance.html` — Governance framework
- `tokenomics.html` — Token economics
- `stack.html` — Technical stack deep-dive
- `style.css` — Design system v7.0 (glass morphism, particle canvas, animated gradients, glow effects)
- `script.js` — Particle system, count-up animations, scroll reveals, language switching

Serve locally: `cd website && python3 -m http.server 3000`
