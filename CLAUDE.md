# CLAUDE.md — Project Context for CURS3D

State as of: **2026-05-04**

## What is this project?

CURS3D is a quantum-resistant Layer 1 blockchain written in Rust from scratch.
It is **not** a fork of any existing chain. Every component — consensus, crypto,
networking, storage, VM, API — is implemented from zero.

## Production Endpoints (live testnet)

| Surface | URL |
|---------|-----|
| Site | https://curs3d.fr |
| API | https://api.curs3d.fr/api/status |
| Explorer | https://explorer.curs3d.fr |
| Faucet UI (Cloudflare Turnstile) | https://curs3d.fr/faucet |
| API docs (OpenAPI 3.1, Stoplight Elements) | https://curs3d.fr/api |
| OpenAPI spec | https://curs3d.fr/api/openapi.json |
| WebSocket | wss://api.curs3d.fr/ws |
| Status (Grafana) | https://status.curs3d.fr/ |
| Status (Uptime-Kuma) | https://status.curs3d.fr/status/ |
| P2P bootnode | 144.24.192.222:4337 |

- **Chain ID:** `curs3d-public-testnet`
- **Genesis hash:** `8a58508589b2e0e2caf760eaed18500c262200fbdff3509faedfc1d9589efb18`
- **Active validators:** **1** (node1 only — see "Known bugs")
- **Validator (node1):** `CURe1Fa551B3f0524EfD8d0673cdBF9fD0e199458c5`
- **Faucet:** `CUR34cafc74B750C0e0150877e99cd27D77C6c4fC44` (100 CUR, 1 h cooldown per address+IP, captcha-gated)

The HTTP API exposes **27 endpoints** (REST + WS + `/eth` JSON-RPC) — see
`/api/openapi.json` for the canonical list. Stoplight Elements renders it at
https://curs3d.fr/api.

## Build & Test

```bash
# Rust nightly is REQUIRED. multiaddr 0.18.2 has type-inference issues
# on stable >= 1.94 that we have not patched out. CI uses nightly.
rustup install nightly --profile minimal
RUSTUP_TOOLCHAIN=nightly cargo build --release

# Tests, lint, format
RUSTUP_TOOLCHAIN=nightly cargo test --lib       # 150 tests across 15 modules
RUSTUP_TOOLCHAIN=nightly cargo clippy --lib -- -D warnings
RUSTUP_TOOLCHAIN=nightly cargo fmt --check
```

`cargo audit` policy lives in `.cargo/audit.toml` — 0 critical findings,
transitive advisories are ignored with per-crate justification.

Edition: 2024.

## Known bugs / open issues

These are documented to spare the next session a re-discovery. None block
the single-validator testnet, but they gate further multi-node rollout.

1. **Consensus slot-leader missing** — `src/consensus/mod.rs` does not
   schedule a single proposer per height. With ≥2 validators running, each
   one produces a block every 10 s, leading to permanent forks. Fix:
   introduce `slot_leader(height, validator_set) -> Address` (deterministic,
   stake-weighted, e.g. `sha3(height || prev_hash)` mod stake-weight) and
   gate block production in `network/mod.rs` on
   `self.address == slot_leader(next_height, ...)`. node2 stays disabled
   until this lands.
2. **RequestBlocks sync timeout** — even with valid peer connectivity, the
   receive loop in `src/network/mod.rs` (BlockResponse path) times out
   before block batches arrive. Investigate the channel/select path and
   per-peer timeouts.
3. **Persisted state-root divergence after some restarts** — diagnostic
   logging now dumps each account leaf when a divergence is detected.
   Root cause not yet identified.
4. **Cross-compile from Mac** — `cross` is installed but needs Docker
   Desktop / OrbStack running. Today, builds happen on the ARM VPSes.

## Project Structure

```
src/
  api/mod.rs           HTTP REST API (hyper 1.x, port 8080)
                       - 27 endpoints + WebSocket (/ws) + Ethereum-compatible JSON-RPC (POST /eth)
                       - Per-IP rate limiting (60 GET/min, 10 POST/min)
                       - Faucet: POST /api/faucet/request (100 CUR, 1h per-address + per-IP cooldown,
                         Cloudflare Turnstile via nginx auth_request → curs3d-captcha.service)
                       - Bearer token auth (optional via CURS3D_API_TOKEN)
                       - CORS configurable (CURS3D_API_ALLOW_ORIGIN)
                       - 128 max HTTP connections, 64 max WebSocket connections, 1MB body limit
                       - Token endpoints: /api/tokens, /api/token/:addr, /api/token/:addr/balance/:owner
                       - Governance endpoints: /api/governance/proposals, /api/governance/proposal/:id
                       - Light client endpoints: /api/genesis, /api/headers, /api/header/:h, /api/finality
                       - Receipts/logs: /api/receipt/:hash, /api/logs (positional topic0..topic3),
                         /api/account/:addr/transactions, /api/block-by-hash/:hash
                       - Contracts: /api/contract/:addr/code (raw bytecode + code_hash + owner)
                       - Hash → height + tx_hash → location indexes for O(1) hash lookups
                       - WebSocket events: new_block, new_header (signed), new_transaction, finality
                       - eth_subscribe over WS: newHeads + logs (Metamask, ethers.js compatible)
  api/eth_rpc.rs       Ethereum-compatible JSON-RPC subset (Metamask, ethers.js, wagmi):
                       eth_chainId, eth_blockNumber, eth_gasPrice, eth_getBalance,
                       eth_getTransactionCount, eth_getCode, eth_getStorageAt,
                       eth_getBlockByNumber/Hash, eth_getTransactionByHash,
                       eth_getTransactionReceipt, eth_getLogs, eth_feeHistory,
                       eth_estimateGas, net_version, web3_clientVersion, web3_sha3.
                       Send-transaction methods reject (ECDSA RLP not supported);
                       use POST /api/tx/submit for native Dilithium-signed txs.
  consensus/mod.rs     BFT PoS, FinalityVote, FinalityTracker, EquivocationEvidence,
                       slashing, epoch rewards, inactivity penalties.
                       NOTE: slot-leader scheduling not implemented — see Known bugs.
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
  wallet/mod.rs        Wallet with AES-256-GCM + Argon2id encryption (m=64MB, t=3, p=4), auto-migration
  lib.rs               Module declarations
  main.rs              CLI entry point (clap 4, includes --reset-p2p-identity flag,
                       genesis subcommand accepts repeated --validator-wallet for multi-validator genesis)

website/               Static site (no style.css/script.js anymore — see Website section)
sdk/
  rust/                  curs3d-contract Rust SDK for writing smart contracts.
                         Compiles to wasm32-unknown-unknown. Bundles a heap-base
                         bump allocator + panic handler. 5 examples:
                         counter, erc20-token, multisig, nft, vesting.
  javascript/            JS SDK
  python/                Python SDK
deploy/
  monitoring/            Docker stack: prometheus + grafana + uptime-kuma + node-exporter
                         (nginx-status.conf serves Grafana at /, Uptime-Kuma at /status/).
  nginx/                 Public TLS config for api.curs3d.fr + explorer.curs3d.fr + curs3d.fr
  scripts/
    add-node.sh           Automated Oracle ARM validator deployment
    setup-node.sh         First-boot bootstrap (creates curs3d user, dirs, units)
    init-localnet.sh      Local 2-validator dev net
    deploy.sh             Deploy compiled binary + units from Mac
    curs3d-healthcheck.sh Healthcheck v2: posts to Discord when restart loops are detected
    curs3d-backup.sh      restic backup → b2:curs3d-backups-pazent:curs3d-node1 (every 6h)
    curs3d-captcha-verify.py
                          Cloudflare Turnstile verifier for the faucet (port 127.0.0.1:8090)
  systemd/
    curs3d.service        Main node unit (EnvironmentFile=/etc/curs3d/secrets.env, hardened)
    curs3d-captcha.service Faucet captcha verifier
    curs3d-backup.service + curs3d-backup.timer  restic to B2 every 6 h
    curs3d-healthcheck.cron */2 min, posts to Discord on restart loops
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
- `MAX_CONTRACT_CODE_BYTES = 262_144` (256 KB hard cap on deployed wasm)
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
- **MISSING:** deterministic slot-leader / per-height proposer election (see Known bugs).

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
- Block production: every 10 seconds (every active validator — should be slot-gated, see Known bugs)
- Height announce: every 30 seconds (signed by validators)
- Sync: batch of 50 blocks, 15s timeout, 3 retries (timeout currently unreliable, see Known bugs)
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
- Faucet: 100 CUR, 1 h cooldown per address + per IP, Cloudflare Turnstile required
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
4. Add the path to `website/api/openapi.json` so the count and `/api` page stay in sync.

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
- Wallet files are encrypted with AES-256-GCM. Argon2id (m=64MB, t=3, p=4) derives the key from password
- `load_auto()` auto-migrates old plaintext wallets to encrypted format
- EIP-1559: base_fee adjusts toward 50% gas target per block
- Epochs freeze validator sets for deterministic selection

## Tests

**150 tests across 15 modules** (post audit hardening 2026-05-04):
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

Run a specific test: `RUSTUP_TOOLCHAIN=nightly cargo test test_name --lib`

## Recent commits (newest first)

- `6ca7603` fix: captcha verifier accepts GET (nginx auth_request default)
- `bbb1443` ops: simplify healthcheck cron — script reads alerts env itself
- `f3e6f98` fix: track src/api/eth_rpc.rs (was untracked, broke clean builds)
- `953eff5` ops: include rest of monitoring stack files
- `2f999d7` ops: cargo audit policy, CI nightly, infra scripts, faucet UI
- `c015ee3` security: fix 11 audit findings (Rust core)

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
- `CURS3D_FAUCET_WALLET` / `CURS3D_FAUCET_PASSWORD_FILE` — Faucet wallet path + password file
- `CURS3D_FAUCET_FEE` — Fee charged for faucet transactions (microtokens)
- `CURS3D_FAUCET_COOLDOWN_FILE` — Per-address cooldown persistence (default: faucet_cooldowns.json)
- `CURS3D_FAUCET_IP_COOLDOWN_FILE` — Per-IP cooldown persistence (default: faucet_ip_cooldowns.json)
- `CURS3D_FAUCET_IP_COOLDOWN_SECS` — Per-IP cooldown duration (default: 3600)
- `CURS3D_FAUCET_REQUIRE_CAPTCHA` — Set to "1" to require captcha verification headers from the reverse proxy
- `CURS3D_FAUCET_CAPTCHA_SECRET` — Shared secret between nginx and node. The proxy must forward
  both `X-Captcha-Verified: 1` AND `X-Captcha-Secret: <this value>` after a successful Cloudflare
  Turnstile check. Constant-time-compared on the node. Without this set, captcha-required mode
  fail-closed (rejects all faucet requests).

On node1, secrets are loaded by systemd via `EnvironmentFile=/etc/curs3d/secrets.env`.
Discord webhooks for ops alerts live in `/etc/curs3d/alerts.env`.

## Website

**Live:** https://curs3d.fr (landing) · https://explorer.curs3d.fr (explorer)

The site is a static bundle in `website/`. The legacy `style.css` / `script.js`
have been removed; the design system is now `landing.css` + `landing.js`.

Pages (bilingual EN/FR):
- `index.html` — Landing page with hero stats and metric strip.
- `run-validator.html` — 10-step guide to operate a validator.
- `docs.html` — Full developer documentation.
- `examples.html` — Step-by-step examples (node, faucet, tokens, WebSocket, SDKs).
- `explorer.html` — Block explorer (defaults to https://api.curs3d.fr, WS live feed).
- `whitepaper.html` — Technical whitepaper.
- `governance.html` — Governance framework.
- `tokenomics.html` — Token economics.
- `stack.html` — Technical stack deep-dive.
- `faucet.html` — Faucet UI behind Cloudflare Turnstile.
- `api.html` + `api/openapi.json` — Stoplight Elements rendering of the OpenAPI 3.1 spec (27 endpoints).

Serve locally: `cd website && python3 -m http.server 3000`.
