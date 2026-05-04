# CLAUDE.md — Project Context for CURS3D

State as of: **2026-05-04** (afternoon — protocol v4 hardfork live)

## What is this project?

CURS3D is a quantum-resistant Layer 1 blockchain written in Rust from scratch.
It is **not** a fork of any existing chain. Every component — consensus, crypto,
networking, storage, VM, API — is implemented from zero.

As of protocol **v4**, the chain runs **two VMs side by side**: the native
quantum-resistant VM (Wasmer 5 WASM, Dilithium-signed transactions) and an
Ethereum-compatible VM (revm 38, secp256k1-signed RLP transactions, MetaMask
/ Hardhat / Foundry / ethers.js compatible). Both VMs share the same state
trie and the same canonical block stream.

## Production Endpoints (live testnet)

| Surface | URL |
|---------|-----|
| Site | https://curs3d.fr |
| API | https://api.curs3d.fr/api/status |
| **Ethereum-compatible JSON-RPC (MetaMask, ethers.js, Hardhat, Foundry)** | **https://api.curs3d.fr/eth** |
| Explorer | https://explorer.curs3d.fr |
| Browser Wallet UI (read-only, see Known issues) | https://curs3d.fr/wallet |
| Wallet WASM bundle (24 KB JS shim + 236 KB WASM) | https://curs3d.fr/wallet-wasm/curs3d_wallet_wasm.js |
| Developers hub | https://curs3d.fr/developers |
| Security (threat model + audit log + bounty) | https://curs3d.fr/security |
| Community | https://curs3d.fr/community |
| Faucet UI (Cloudflare Turnstile) | https://curs3d.fr/faucet |
| API docs (OpenAPI 3.1, Stoplight Elements) | https://curs3d.fr/api |
| OpenAPI spec | https://curs3d.fr/api/openapi.json |
| WebSocket | wss://api.curs3d.fr/ws |
| Status (Grafana) | https://status.curs3d.fr/ |
| Status (Uptime-Kuma) | https://status.curs3d.fr/status/ |
| Sitemap (14 URLs, hreflang en/fr) | https://curs3d.fr/sitemap.xml |
| security.txt (RFC 9116) | https://curs3d.fr/.well-known/security.txt |
| 404 page | https://curs3d.fr/404.html |
| OG social card (1200×630) | https://curs3d.fr/og-image.svg |
| P2P bootnode | 144.24.192.222:4337 |

- **Chain ID:** `curs3d-public-testnet`
- **Protocol version:** **v4** (added EVM dispatch + slot-leader scheduling)
- **Genesis hash (v4 redeploy):** `daeb2e6ac802c182ab737d11211339a3d8c3a8da8f2dfd56595e319dbc938b23`
- **Active validators:** **2** (node1 + node2 — both producing and finalizing thanks to slot-leader)
- **Validator (node1):** `CURe1Fa551B3f0524EfD8d0673cdBF9fD0e199458c5`
- **Validator (node2):** `CURdC1ecceD4f12Cb3E34BD0d43E72d6D04fC4823dd`
- **Faucet:** `CUR34cafc74B750C0e0150877e99cd27D77C6c4fC44` (100 CUR, 1 h cooldown per address+IP, captcha-gated)

The HTTP API exposes **27 endpoints** (REST + WS + `/eth` JSON-RPC) — see
`/api/openapi.json` for the canonical list. Stoplight Elements renders it at
https://curs3d.fr/api.

### MetaMask / Hardhat / Foundry network config

| Field | Value |
|-------|-------|
| RPC URL | `https://api.curs3d.fr/eth` |
| Chain ID (decimal) | `1800329576` |
| Chain ID (hex) | `0x6b4ed968` |
| Symbol | `CUR` |
| Block explorer | `https://explorer.curs3d.fr` |

## Build & Test

```bash
# Rust nightly is REQUIRED. multiaddr 0.18.2 has type-inference issues
# on stable >= 1.94 that we have not patched out. CI uses nightly.
rustup install nightly --profile minimal
RUSTUP_TOOLCHAIN=nightly cargo build --release

# Tests, lint, format
RUSTUP_TOOLCHAIN=nightly cargo test --lib       # 164 tests, all green
RUSTUP_TOOLCHAIN=nightly cargo clippy --lib -- -D warnings
RUSTUP_TOOLCHAIN=nightly cargo fmt --check
```

> **Note:** revm 38 (added in `c34b366`) pulls ~200 new transitive crates.
> First clean build on Oracle ARM Free Tier takes 5–8 minutes; subsequent
> incremental builds are ~1 minute.

`cargo audit` policy lives in `.cargo/audit.toml` — 0 critical findings,
transitive advisories are ignored with per-crate justification.

Edition: 2024.

## Known bugs / open issues

These are documented to spare the next session a re-discovery. None block
the public 2-validator testnet, but they affect specific surfaces.

1. **Browser wallet UI is read-only.** ML-DSA-87 (FIPS-204 finalized, used in
   the wasm bundle, RustCrypto `ml-dsa` crate) is **not** byte-compatible
   with `pqcrypto-dilithium 0.5.0` (NIST round 3, used by the node). Public
   key and signature sizes match exactly (PK 2592 B, sig 4627 B) but the
   challenge sampling and message framing differ (FIPS-204 uses
   `M' = 0x00 || ctx_len || ctx || M`, round-3 doesn't). Signatures produced
   in the browser are silently rejected by `/api/tx/submit`. The wallet
   therefore displays balance / nonce / staked / tx history but **cannot
   sign or send**. Fix path: migrate the node from `pqcrypto-dilithium` to
   `pqcrypto-mldsa` or `ml-dsa` (RustCrypto). That migration is itself a
   crypto-core hardfork.
2. **`wasm-opt` failed during `wasm-pack build`** on the build host (no
   recent binaryen). The deployed bundle is 236 KB instead of ~100 KB
   optimized. Workaround: `brew install binaryen` (or `apt install
   binaryen`) and rerun, OR set `wasm-opt = false` in
   `sdk/wasm/Cargo.toml [package.metadata.wasm-pack.profile.release]` to
   silence the warning.
3. **HTML/CSS/JS a11y + SEO polish on existing pages** was prototyped in a
   worktree but not merged due to conflicts with the wallet-nav additions.
   Will be reapplied in a follow-up pass.
4. **RequestBlocks sync timeout** — `src/network/mod.rs` BlockResponse path.
   The bug is still in the code but no longer triggers in normal operation
   thanks to deterministic slot-leader scheduling (commit `343a7a1`).
5. **Persisted state-root divergence after some restarts** — diagnostic
   logging now dumps each account leaf when a divergence is detected.
   Root cause not yet identified. Rare in practice.
6. **Cross-compile from Mac** — `cross` is installed but needs Docker
   Desktop / OrbStack running. Today, builds happen on the ARM VPSes.
7. **No PGP key for security disclosures yet.** A signed contact channel
   is a TODO. Until it is published, security issues are reported privately
   via GitHub security advisories on `Pazificateur69/curs3d`. The plan is to
   publish a long-lived PGP key under `/.well-known/security.txt`.
8. **No external security audit.** All cryptography, consensus and VM code
   is implemented in-house and reviewed only internally. External audit is
   a prerequisite to mainnet, not to the public testnet.

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
  api/eth_rpc.rs       Ethereum-compatible JSON-RPC (Metamask, ethers.js, wagmi, Hardhat,
                       Foundry). Read methods: eth_chainId, eth_blockNumber, eth_gasPrice,
                       eth_getBalance, eth_getTransactionCount, eth_getCode,
                       eth_getStorageAt, eth_getBlockByNumber/Hash, eth_getTransactionByHash,
                       eth_getTransactionReceipt, eth_getLogs, eth_feeHistory,
                       eth_estimateGas, net_version, web3_clientVersion, web3_sha3.
                       **Write method (v4):** eth_sendRawTransaction now accepts RLP-
                       encoded secp256k1-signed transactions, decodes the legacy/EIP-1559
                       payload, recovers the sender via secp256k1, and dispatches to the
                       EVM via TransactionKind::DeployEvmContract / CallEvmContract.
                       POST /api/tx/submit remains the path for native Dilithium-signed txs.
  consensus/mod.rs     BFT PoS, FinalityVote, FinalityTracker, EquivocationEvidence,
                       slashing, epoch rewards, inactivity penalties,
                       deterministic stake-weighted slot-leader scheduling (v4 — commit 343a7a1).
  core/
    block.rs           BlockHeader, Block, genesis, signatures, verification
    blocktree.rs       BlockTree, fork choice (heaviest chain), pruning
    chain.rs           Blockchain struct, state management, validation, reorg, fee market, snapshots, token/governance dispatch
    transaction.rs     Transaction, 14 types: Transfer, Stake, Unstake, Coinbase, DeployContract, CallContract, DeployToken, TokenTransfer, TokenApprove, TokenTransferFrom, SubmitProposal, GovernanceVote, **DeployEvmContract**, **CallEvmContract** (last two appended at end of enum for bincode compat). Adds `evm_raw_tx: Vec<u8>` field carrying the original RLP payload for EVM dispatch.
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
    mod.rs             Wasmer 5 WASM execution, Cranelift, fuel middleware, 11 host functions (native CURS3D contracts)
    evm.rs             revm 38 integration (~1066 LOC) — Solidity / MetaMask compat. Shares the same state trie as the native VM. Activated in v4 hardfork (commit c34b366).
    gas.rs             Gas cost schedule
    state.rs           ContractState (code, storage, owner)
  wallet/mod.rs        Wallet with AES-256-GCM + Argon2id encryption (m=64MB, t=3, p=4), auto-migration
  lib.rs               Module declarations
  main.rs              CLI entry point (clap 4, includes --reset-p2p-identity flag,
                       genesis subcommand accepts repeated --validator-wallet for multi-validator genesis)

website/               Static site (no style.css/script.js anymore — see Website section).
                       wallet.html (37 KB) + wallet.js (38 KB) — browser wallet UI.
                       New SEO/utility pages: developers.html, security.html,
                       community.html, 404.html, og-image.svg, sitemap.xml,
                       robots.txt, .well-known/security.txt (all linked at top).
sdk/
  rust/                  curs3d-contract Rust SDK for writing smart contracts.
                         Compiles to wasm32-unknown-unknown. Bundles a heap-base
                         bump allocator + panic handler. 5 examples:
                         counter, erc20-token, multisig, nft, vesting.
  wasm/                  curs3d-wallet-wasm — browser-side crypto bundle (commit b896ede).
                         Standalone crate, EXCLUDED from the root workspace
                         (no top-level [workspace] table at the repo root, so
                         `cargo build` from root won't try to host-build it).
                         Uses `ml-dsa` (FIPS-204 ML-DSA-87) + `argon2` (Argon2id
                         m=64MiB, t=3, p=4) + `aes-gcm` (AES-256-GCM) + `sha3`.
                         Output bundle: 24 KB JS shim + 236 KB WASM (no wasm-opt;
                         see Known issues #2). 12 native Rust tests, all green.
                         Powers https://curs3d.fr/wallet. **Read-only** today
                         (Dilithium dialect mismatch — see Known issues #1).
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
- **Slot-leader (v4):** `slot_leader(height, validator_set) -> Address` —
  deterministic, stake-weighted, derived from `sha3(height || prev_hash)`
  modulo cumulative stake. Block production in `network/mod.rs` is gated on
  `self.address == slot_leader(next_height, ...)`. Multi-validator forks at
  every height are eliminated. Commit `343a7a1`.

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
- Block production: every 10 seconds, gated by `slot_leader(next_height, ...)` (v4)
- Height announce: every 30 seconds (signed by validators)
- Sync: batch of 50 blocks, 15s timeout, 3 retries (latent bug, see Known bugs #4)
- Network topic: derived from chain_id + protocol_version. Commit `6dcafbf`
  pins `protocol_version_at_height(0)` to the active baseline so the gossipsub
  topic is stable from genesis instead of churning on the first upgrade boundary.

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

**164 tests, all green** (2026-05-04 afternoon, post v4 hardfork). Breakdown
below is approximate — the +14 tests since the previous 150-test baseline
mostly cover EVM dispatch (revm 38 integration), slot-leader determinism, and
EVM transaction encode/decode round-trips. Run `cargo test --lib --no-run` and
read the binary output for the canonical per-module count.
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

- `9000b1b` fix(main): set `evm_raw_tx: Vec::new()` on remaining Transaction literals
- `a1a4a16` website: SEO + new pages (developers / security / community / 404)
- `b896ede` sdk/wasm: browser-side crypto bundle (Dilithium / ML-DSA + AES-GCM + Argon2id)
- `a26c4bb` website: browser wallet UI (locked / unlocked / send / history)
- `c34b366` vm/evm: integrate revm 38 as second VM (Solidity / MetaMask compat) — **v4 hardfork**
- `6dcafbf` fix(consensus): return current protocol version at height 0 too — stable gossipsub topic
- `343a7a1` consensus: deterministic stake-weighted slot-leader scheduling
- `e547331` ops: misc deploy improvements (start-limit, --http-addr, init helper)
- `b4e9de6` site+sdk: track production assets (was rsynced to prod, never in git)
- `7c6f2d9` docs: full sync to 2026-05-04 production state (previous baseline)

## Dependencies (key ones)

- `pqcrypto-dilithium` — Post-quantum signatures, Dilithium Level 5 (NIST round 3)
- `sha3` — Keccak hashing
- `sled` — Embedded key-value database
- `libp2p` 0.54 — P2P networking (Gossipsub + mDNS + noise + yamux)
- `hyper` 1.x — HTTP server
- `wasmer` 5 + `wasmer-types` 5 — Native CURS3D WASM VM with Cranelift
- `revm` 38 — Ethereum VM (Solidity / MetaMask), v4 hardfork
- `secp256k1` + `rlp` — EVM transaction recovery and decoding
- `aes-gcm` + `argon2` — Wallet encryption
- `clap` 4 — CLI parsing
- `tokio` — Async runtime
- `serde` + `bincode` — Serialization
- `chrono` — Timestamps
- `thiserror` — Error types
- `tracing` — Logging

`sdk/wasm/Cargo.toml` (separate crate, wasm32-unknown-unknown target):
- `ml-dsa` 0.1.0-rc.9 — RustCrypto FIPS-204 ML-DSA-87
- `argon2`, `aes-gcm`, `sha3`, `bincode`, `wasm-bindgen`

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

Pages (bilingual EN/FR via `data-lang` blocks — do not strip the structure
when editing):
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
- **`wallet.html` + `wallet.js` — Browser wallet UI (read-only, see Known issues #1).**
- **`developers.html` — Developers hub (SDKs, MetaMask config, sample contracts).**
- **`security.html` — Threat model, audit log, bug-bounty path.**
- **`community.html` — GitHub / Discord / X / newsletter.**
- **`404.html` — Branded 404 page.**
- `api.html` + `api/openapi.json` — Stoplight Elements rendering of the OpenAPI 3.1 spec (27 endpoints).
- `sitemap.xml` (14 URLs, hreflang en/fr), `robots.txt` (allow-all),
  `og-image.svg` (1200×630 social card), `.well-known/security.txt` (RFC 9116).

Wallet WASM bundle is served from `/var/www/curs3d/wallet-wasm/` via nginx.
The wallet's CSP needs `wasm-unsafe-eval` in `script-src` for instantiate;
the `curs3d.fr` vhost was updated accordingly.

Serve locally: `cd website && python3 -m http.server 3000`.

## Hardfork procedure (v3 → v4 and future protocol bumps)

The v4 hardfork bundles three breaking changes:

1. **EVM dispatch** (revm 38 alongside Wasmer)
2. **Slot-leader stake-weighted scheduling**
3. **EVM-flavored transactions** (RLP-signed, secp256k1 sender recovery)

Procedural notes:

- The genesis itself does **not** include explicit upgrades — chains are
  generated with `protocol_version_at_height(0) = 4` uniformly. Mixed-version
  peers diverge silently. Coordinate restarts.
- Chain DBs from v3 or earlier are **not** forwards-compatible; full wipe of
  `/var/lib/curs3d/` is required. Validator wallet, faucet wallet,
  `p2p_identity.pb`, and password files must be preserved.
- `TransactionKind::DeployEvmContract` and `CallEvmContract` are appended at
  the end of the enum so bincode discriminants for older variants are
  preserved (forward-compatible bincode payloads, but the *content* of an
  EVM tx requires v4 to apply).
