<p align="center">
  <br>
  <strong style="font-size: 2rem;">CURS3D</strong><br>
  <em>Quantum-Resistant Layer 1 Blockchain</em>
  <br><br>
  <a href="https://github.com/Pazificateur69/curs3d/actions"><img src="https://github.com/Pazificateur69/curs3d/workflows/CI/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-2024_edition-orange.svg" alt="Rust 2024"></a>
  <img src="https://img.shields.io/badge/tests-cargo%20test-brightgreen.svg" alt="cargo test">
  <img src="https://img.shields.io/badge/clippy-0%20warnings-brightgreen.svg" alt="0 clippy warnings">
  <img src="https://img.shields.io/badge/quantum-resistant-blueviolet.svg" alt="Quantum Resistant">
  <a href="https://api.curs3d.fr/api/status"><img src="https://img.shields.io/badge/testnet-LIVE-brightgreen.svg" alt="Testnet Live"></a>
  <img src="https://img.shields.io/badge/version-v0.3.5-informational.svg" alt="Software v0.3.5">
  <br>
  <a href="https://curs3d.fr">Website</a> · <a href="https://curs3d.fr/docs">Docs</a> · <a href="https://curs3d.fr/whitepaper">Whitepaper</a> · <a href="https://explorer.curs3d.fr">Explorer</a> · <a href="https://curs3d.fr/examples">Tutorials</a> · <a href="https://api.curs3d.fr/api/status">Live API</a>
</p>

---

> ## :warning: Status: experimental devnet — v0.3.5 (pre-mainnet)
>
> **Software version `v0.3.5`** — early-stage testnet. **`v1.0` is reserved for the official mainnet launch** when external audit, governance bootstrap, and feature-completeness are done. The number you see prefixed with `v0.` is intentional: this is a working developer testnet, not a finished product.
>
> Separately, the **consensus protocol version is `v5`** — that is an internal compatibility marker between nodes, not a marketing version of the project itself. Do not confuse the two: software `v0.3.5` runs consensus protocol `v5`.
>
> This is an **experimental developer testnet** for development and testing. **It is not production.** Funds on this chain have **no monetary value**. The chain may be reset without notice. The browser wallet UI is write-capable: create or import a wallet, sign and send transactions, all from the browser via the embedded WASM crypto. The private key never leaves your device. External security audit is **not yet started**. Do not use CURS3D for anything you cannot afford to lose.
>
> **Production incident — storage fix in current tree:** the first soak caught two cascading issues. Root-cause-fixed, not papered over.
> 1. **sled 0.34 internal deadlock** under sustained writes (confirmed twice by gdb on live hung nodes). The short-term epoch-boundary mitigation was not enough, so persistent storage has been migrated to **redb 4.1**. Live nodes also use async persistence, so consensus/RPC/gossipsub no longer perform disk IO while holding `Mutex<Blockchain>`.
> 2. **Mesh topology depended on node1 as gossipsub relay** — node2 and node3 only had node1 as `--bootnode`, so when node1's sled deadlock starved its gossipsub task, the mesh partitioned and the chain forked at h=341. Fix: each systemd unit now lists the OTHER two nodes as bootnodes; full mesh, no single SPOF.
>
> Live rollout completed 2026-05-06: redb deployed via `full-rollout.sh --wipe`, finality active at h=32, then chain producing steadily. **Solidity portfolio redeployed 2026-05-07** with a fresh deployer keystore (the previous keystore password was lost; a new one was generated and saved at `~/.curs3d/deployer.password` on the operator Mac, see `docs/SECRETS.md`); the seven contract addresses in `contracts/deployments/1800329576.json` are the live ones. Pre-2026-05-06 contract addresses are dead. Full incident log + gdb stack + recipe in [`docs/foundation/22-soak-runbook.md`](docs/foundation/22-soak-runbook.md).

CURS3D is a **Layer 1 blockchain written from scratch in Rust**, designed to resist quantum computing attacks. It uses **NIST FIPS-204 ML-DSA-87** (final standardised version of CRYSTALS-Dilithium-L5) for native signatures, BFT Proof of Stake consensus with explicit 2/3 finality, deterministic stake-weighted slot-leader scheduling, an EIP-1559 dynamic fee market, and a **dual-VM execution layer**: a native WASM engine (Wasmer 7) and an Ethereum-compatible VM (revm 38) sharing the same state trie. Every native component is original — no fork of Ethereum, Cosmos, or Substrate.

> **MetaMask works.** Point your wallet at `https://rpc.curs3d.fr/eth` (or `https://api.curs3d.fr/eth`), chain ID `1800329576`, and you can deploy Solidity, send ETH-style txs, sign with ethers.js / wagmi, and use Hardhat / Foundry against the live testnet. EVM transactions are signed with secp256k1 ECDSA (standard Ethereum) and accepted by design — that's how MetaMask compat works. Native CURS3D transactions (Stake / governance / native deploy) sign with ML-DSA-87 and go through `POST /api/tx/submit`. Both families produce blocks on the same chain.

> **Status (2026-05-21 — software v0.3.5, consensus protocol v5, 5-validator testnet):** chain regenerated fresh at h=0 on 2026-05-21 with **5 validators across 4 providers** (Oracle Cloud Free Marseille ×2 ARM, IONOS Berlin x86, Hostinger Plesk ×2 x86 — total 250 000 CUR staked). New genesis SHA-256 `e830418885dd9057f9f44d4f409ba8bbccf536be9efd3b519017a5319d3b59af`. The previous 2-validator chain (regen 2026-05-06, SHA `165c5f9d2a77719ecada5937753465806d83429588df06f0f25cea5c274bbf4e`) hit a fork-pollution incident on 2026-05-20 caused by a forgotten Plesk stealth node running an older genesis; rather than salvage the 8500-block history we restarted from a clean genesis with twice the BFT tolerance (5-validator chain tolerates 1-2 validators down before finality stops). Browser wallet UI ([curs3d.fr/wallet](https://curs3d.fr/wallet)) signs ML-DSA-87 transactions natively via the WASM bundle.

### MetaMask / Hardhat / Foundry network config

| Field | Value |
|-------|-------|
| RPC URL | `https://rpc.curs3d.fr/eth` (`https://api.curs3d.fr/eth` also works) |
| Chain ID (decimal) | `1800329576` |
| Chain ID (hex) | `0x6b4ed968` |
| Symbol | `CUR` |
| Block explorer | `https://explorer.curs3d.fr` |

## Why CURS3D?

**The quantum threat is real.** NIST finalized post-quantum cryptography standards in 2024. Most blockchains still rely on ECDSA/EdDSA, which will be broken by Shor's algorithm. CURS3D is built from the ground up with quantum-resistant primitives — not retrofitted.

| What | How |
|------|-----|
| **Native signatures** | ML-DSA-87 (FIPS-204 final standard, Dilithium Level 5 security) |
| **EVM signatures** | secp256k1 ECDSA (RLP, MetaMask, Hardhat, Foundry) — non-quantum-resistant by design, gated to the EVM surface |
| **Hashing** | SHA-3 Keccak-256, double-hash blocks, Merkle trees |
| **Wallet encryption** | AES-256-GCM + Argon2 KDF (m=64MiB, t=3, p=4) |
| **Consensus** | BFT Proof of Stake, 2/3 stake-weighted finality |
| **Slot leader** | Deterministic stake-weighted, `sha3(height \|\| prev_hash) % cumulative_stake` |
| **Native VM** | WASM (Wasmer 7 + Cranelift), per-instruction fuel metering |
| **EVM** | revm 38, shares the same state trie as the native VM (added in v4 hardfork, current protocol is v5) |
| **Fee market** | EIP-1559 dynamic base fee, priority fees, gas refunds |
| **Fork choice** | Heaviest chain by cumulative proposer stake |
| **Slashing** | Cryptographic equivocation proof, 33% penalty, 64-block jail |
| **Networking** | libp2p 0.54 (Gossipsub + mDNS + noise + yamux) |
| **Storage** | redb embedded DB, schema v4, auto-migration |

## Quick Start

> **Rust toolchain:** nightly is required. `multiaddr 0.18.2` fails to compile
> on stable ≥ 1.94 due to a type-inference regression we have not patched out.
> Use `rustup install nightly --profile minimal` and prefix builds with
> `RUSTUP_TOOLCHAIN=nightly`.

```bash
# Build from source
git clone https://github.com/Pazificateur69/curs3d.git
cd curs3d
RUSTUP_TOOLCHAIN=nightly cargo build --release

# Create password files for non-interactive deploys
printf '%s\n' 'change-this-validator-password' > validator.password

# Create an encrypted wallet (CRYSTALS-Dilithium 5 keypair)
./target/release/curs3d wallet --output validator.json --password-file validator.password

# Generate a real public testnet genesis from the wallet you will operate
./target/release/curs3d genesis \
  --output genesis.public-testnet.json \
  --validator-wallet validator.json \
  --validator-password-file validator.password

# Publish a stable bootnode address for your VPS
./target/release/curs3d bootnode-address \
  --data-dir curs3d_data \
  --public-addr /dns4/node.example.com/tcp/4337

# Run a validator node
./target/release/curs3d node \
  --validator-wallet validator.json \
  --validator-password-file validator.password \
  --genesis-config genesis.public-testnet.json \
  --public-addr /dns4/node.example.com/tcp/4337

# The node exposes:
#   P2P:      0.0.0.0:4337  (Gossipsub + mDNS)
#   HTTP API: 127.0.0.1:8080
#   TCP RPC:  127.0.0.1:9545

# Check chain status (local)
curl http://localhost:8080/api/status | jq .data

# Or query the live public testnet directly
curl https://api.curs3d.fr/api/status | jq .data
```

### Live Public Testnet

The CURS3D public testnet is running and accessible:

| Surface | URL |
|---------|-----|
| **Site** | https://curs3d.fr |
| **API** | https://api.curs3d.fr/api/status |
| **Ethereum-compatible JSON-RPC** | `https://rpc.curs3d.fr/eth` (`https://api.curs3d.fr/eth` also works) |
| **Explorer** | https://explorer.curs3d.fr |
| **Browser Wallet UI** (write-capable since v5: ML-DSA-87 in-browser signing) | https://curs3d.fr/wallet |
| **Developers hub** | https://curs3d.fr/developers |
| **Security (threat model + audit log + bounty)** | https://curs3d.fr/security |
| **Community** | https://curs3d.fr/community |
| **Faucet UI** | https://curs3d.fr/faucet (Cloudflare Turnstile, 100 CUR, 1 h cooldown) |
| **API docs (OpenAPI 3.1)** | https://curs3d.fr/api |
| **WebSocket** | `wss://api.curs3d.fr/ws` |
| **Status (Grafana)** | https://status.curs3d.fr/ |
| **Status (Uptime-Kuma)** | https://status.curs3d.fr/status/ |
| **P2P Bootnode** | `144.24.192.222:4337` |
| **Chain ID (string)** | `curs3d-public-testnet` |
| **Chain ID (EVM, decimal)** | `1800329576` |
| **Chain ID (EVM, hex)** | `0x6b4ed968` |
| **Genesis hash (v5, regen 2026-05-05)** | `81420887fb59cd7c4837b2195bedbbb78291bd835e5b72162337f10d26f315d6` |
| **Protocol version** | `v5` (ML-DSA-87 / FIPS-204 + EVM + slot-leader) |
| **Active validators** | 5 across 4 providers (Oracle Cloud ARM Marseille ×2, IONOS Berlin x86_64, Hostinger Plesk x86_64 ×2 stealth), each 20% stake = 50 000 CUR |

```bash
# Request testnet tokens
curl -X POST https://api.curs3d.fr/api/faucet/request \
  -H 'Content-Type: application/json' \
  -d '{"address":"YOUR_CUR_ADDRESS"}'

# View recent blocks
curl https://api.curs3d.fr/api/blocks?from=0\&limit=5 | jq .data

# View active validators
curl https://api.curs3d.fr/api/validators | jq .data
```

### With Docker

```bash
docker compose up -d             # Bootstraps a real 2-validator localnet
docker compose logs -f node1     # Watch node startup
curl localhost:8080/api/status   # Query validator 1
curl localhost:8081/api/status   # Query validator 2
docker compose down              # Stop
```

### Deploy Your Own Node

Use the deployment assets in [`deploy/`](deploy/):

- [`deploy/scripts/rollout-staggered.sh`](deploy/scripts/rollout-staggered.sh) — **Default** zero-downtime rollout: scp pre-built binaries, restart `node3 → node2 → node1` one at a time with health gates between each.
- [`deploy/scripts/full-rollout.sh`](deploy/scripts/full-rollout.sh) — Coordinated cold restart for storage-format migrations or hardforks (`--wipe` optional).
- [`deploy/scripts/deploy.sh`](deploy/scripts/deploy.sh) — One-shot single-VPS bootstrap.
- [`deploy/docker-compose.public.yml`](deploy/docker-compose.public.yml)
- [`deploy/systemd/curs3d.service`](deploy/systemd/curs3d.service) — Template (per-node units with mutual bootnodes are in the same dir)
- [`deploy/nginx/curs3d.conf`](deploy/nginx/curs3d.conf)
- Operational runbook: [`deploy/DEPLOY_RUNBOOK.md`](deploy/DEPLOY_RUNBOOK.md)

For routine code changes, the default flow is **(1) cross-compile from your Mac with `cross` + Docker/OrbStack → (2) `./deploy/scripts/rollout-staggered.sh`**. The mutual-bootnode mesh keeps 4 of 5 validators producing throughout, so the chain never goes dark during the upgrade.

## What's Built (Current State)

CURS3D is an **advanced L1 prototype** — not yet mainnet-ready, but technically substantial. Here's what exists in the codebase today, all tested:

- **BFT PoS consensus** with epoch-frozen validator sets, deterministic stake-weighted slot-leader, and 2/3 finality threshold
- **Dual VM**: native WASM (Wasmer 7 + Cranelift, 11 host functions, instruction-level fuel) **and** Ethereum (revm 38, MetaMask / Solidity / Hardhat / Foundry) sharing the same state trie
- **EIP-1559 fee market** with dynamic base fee, separate max/priority fees, gas refunds, mempool pressure management
- **14 transaction types**: native transfers/staking, native WASM contracts, CUR-20 token ops, governance, **EVM deploy / call** (last two appended at the end of the enum to preserve bincode discriminants)
- **CUR-20 token standard**: deploy, transfer, approve, transferFrom with native registry
- **On-chain governance**: validator proposals, stake-weighted voting, automatic execution
- **Light client** module with header-only sync and Merkle proof verification
- **WebSocket** real-time event streaming (new blocks, transactions, finality)
- **Fork choice tree** with heaviest-chain rule, automatic reorg, finality boundary, non-canonical pruning
- **Provable slashing** with cryptographic EquivocationEvidence (dual Dilithium signatures)
- **State sync** with Merkle-verified snapshot chunks, manifest protocol, finalized checkpoints
- **Account + storage proofs** exportable via API (Merkle inclusion proofs)
- **Encrypted wallets** (AES-256-GCM + Argon2), auto-migration from legacy format
- **Protocol versioning** with upgrade-at-height activation and network topic filtering
- **P2P rate limiting** with per-peer tracking, escalating bans, automatic cleanup
- **Peer scoring** reputation system with behavior-based scoring and automatic bans
- **Epoch rewards** distributed to validators proportional to stake and blocks produced
- **Inactivity penalties** with grace period, then escalating stake deductions
- **Sparse Merkle Trie** with 256-bit key space, O(log n) proofs (ready for state root migration)
- **Domain separation** in all hashing (prevents cross-layer collisions)
- **Checksummed addresses** (EIP-55 style, detects typos)
- **Rate-limit headers** (X-RateLimit-Limit/Remaining/Window) on all API responses
- **Persistent storage** (redb, 10 tables, schema v4 with auto-migration)
- **REST API** (27 endpoints, OpenAPI 3.1 at https://curs3d.fr/api) + WebSocket + **Ethereum-compatible JSON-RPC** (`POST https://rpc.curs3d.fr/eth`, full `eth_sendRawTransaction` + log/receipt/block reads) + TCP RPC + CLI
- **SDKs**: JavaScript/TypeScript (@curs3d/sdk), Python (curs3d), Rust contract SDK with 5 examples, **`sdk/wasm` browser-side crypto bundle** (24 KB JS + 236 KB WASM, ML-DSA-87 + Argon2id + AES-GCM)
- **Browser Wallet UI** at [curs3d.fr/wallet](https://curs3d.fr/wallet) — keypair gen, encrypted local storage (Argon2id+AES-GCM), balance / nonce / staked / tx history, **and full signing** (ML-DSA-87 in-browser via the WASM crypto bundle, byte-compatible with the node since v5)
- **Block explorer** web UI with live dashboard
- **Static site additions:** [/developers](https://curs3d.fr/developers), [/security](https://curs3d.fr/security), [/community](https://curs3d.fr/community), 404 page, sitemap (hreflang en/fr), security.txt (RFC 9116), OG card
- **Benchmarks** (criterion) and **fuzzing** targets (cargo-fuzz)
- **Docker** multi-stage build + docker-compose + nginx TLS + systemd
- **CI/CD** pipeline on Rust nightly (check, test, clippy 0 warnings, fmt, cargo audit)
- **181 tests** passing on `cargo test --lib`

### What Remains for Mainnet

- External security audit (consensus, native VM, EVM, crypto)
- Migrate state root to Sparse Merkle Trie (protocol upgrade)
- Long-run soak tests + partition testing
- Contract SDK (Rust + AssemblyScript)
- Tokenomics finalisation (issuance schedule, fee market parameters, validator program)

## Known Issues

These are tracked in [`CLAUDE.md`](CLAUDE.md) and reproduced here so anyone running a node, building against the API, or evaluating CURS3D sees them up front. None block the public 5-validator testnet, but they do shape what is and isn't safe to rely on today.

- **External security audit not yet performed.** Internal audit cycles (3-AI council 2026-04, Codex passes 2026-05) have closed many findings, but no third-party firm has reviewed the codebase. Treat this testnet accordingly.
- **State-root divergence at epoch boundaries — fixed in `f461aa4`.** Root cause: epoch settlement applied at block-apply time but skipped at boot replay; identical helper now runs in both paths. Regression test added (`test_restart_across_epoch_boundary`).
- **Wallet read-only — fixed in `59694cb` (v5 hardfork).** Node migrated from `pqcrypto-dilithium 0.5` (NIST round 3) to `ml-dsa = 0.1.0-rc.9` (FIPS-204), the same crate the browser wallet uses. Signatures are byte-compatible across both sides; the wallet UI signs and sends transactions natively.
- **`RequestBlocks` sync timeout / boot forks — fixed in current tree.** Block responses now tolerate stale batches, sync escalates to snapshot instead of silently giving up, forked `RequestBlocks` callers receive a snapshot offer, and validators pause production during startup/sync so they do not create isolated first blocks before the peer mesh forms. Nodes also exchange verified public peer addresses in signed `HeightAnnounce` messages and persist them in `peerstore.json`, so a new validator only needs one reachable bootnode once; the mesh learns and redials it automatically after restarts. Regression coverage: `test_two_node_cold_sync_via_request_blocks`, `test_two_node_cold_sync_100_blocks_multi_batch`, `test_initial_production_gate_waits_for_peer_mesh`, `test_genesis_backup_rank_uses_node_start_anchor`, `test_height_announce_legacy_json_defaults_public_addrs`, `test_peerstore_reloads_public_peer_addrs`.
- **No PGP key for security disclosures yet.** A signed contact channel is a TODO. Until then, please report security issues privately via GitHub security advisories on `Pazificateur69/curs3d`.

## Architecture

```
src/
  api/             HTTP REST API (hyper 1.x, 128 max connections, 1MB body limit)
  consensus/       BFT PoS, FinalityVote, FinalityTracker, EquivocationEvidence, slashing
  core/
    block.rs         BlockHeader, Block, genesis, signatures, verification
    blocktree.rs     BlockTree, fork choice (heaviest chain), pruning
    chain.rs         Blockchain state, validation, reorg, fee market, snapshots
    transaction.rs   14 types: Transfer, Stake, Unstake, Coinbase, DeployContract, CallContract, DeployToken, TokenTransfer, TokenApprove, TokenTransferFrom, SubmitProposal, GovernanceVote, DeployEvmContract, CallEvmContract
    receipt.rs       Execution receipts with gas details and logs
    state_proof.rs   AccountProof, StorageProof (Merkle inclusion)
  crypto/
    dilithium.rs     CRYSTALS-Dilithium Level 5 (pqcrypto)
    hash.rs          SHA-3, double-hash, Merkle trees/proofs, address derivation
  governance/      On-chain governance: proposals, voting, parameter changes
  light/           Light client: header sync, Merkle proof verification
  network/         libp2p P2P (Gossipsub + mDNS), sync, block production, state sync, rate limiting
  rpc/             TCP JSON RPC (port 9545)
  storage/         redb DB (10 tables, schema v4, snapshots, migration)
  token/           CUR-20 token standard: deploy, transfer, approve, transferFrom
  vm/
    mod.rs           Wasmer 7 WASM execution, host functions, fuel middleware (native CURS3D contracts; bumped from 5 to 7 on 2026-05-05 to fix x86_64 linker)
    evm.rs           revm 38 (Solidity / MetaMask) — added in v4 hardfork
    gas.rs           Gas cost schedule
    state.rs         ContractState (code, storage, owner)
  wallet/          Encrypted wallet (AES-256-GCM + Argon2)
  main.rs          CLI entry point (clap 4)

website/           Static site (landing, docs, wallet UI, developers, security, community, ...)
sdk/wasm/          Browser-side crypto bundle (ML-DSA-87 + Argon2id + AES-GCM); standalone crate, excluded from the root workspace
```

## REST API + Ethereum-compatible JSON-RPC

The full machine-readable contract is the OpenAPI 3.1 spec at
[`website/api/openapi.json`](website/api/openapi.json) (also live at
https://curs3d.fr/api/openapi.json). It currently documents **27 endpoints**.

The Ethereum-compatible JSON-RPC is exposed at `POST /eth` (live at
`https://rpc.curs3d.fr/eth` and `https://api.curs3d.fr/eth`) and supports MetaMask, ethers.js, wagmi, viem,
Hardhat, and Foundry. Read methods cover `eth_chainId`, `eth_blockNumber`,
`eth_gasPrice`, `eth_getBalance`, `eth_getTransactionCount`, `eth_getCode`,
`eth_getStorageAt`, `eth_getBlockByNumber`/`Hash`, `eth_getTransactionByHash`,
`eth_getTransactionReceipt`, `eth_getLogs`, `eth_feeHistory`,
`eth_estimateGas`, `net_version`, `web3_clientVersion`, `web3_sha3`.
**Write method:** `eth_sendRawTransaction` accepts RLP-encoded
secp256k1-signed transactions, recovers the sender, and dispatches to the
EVM. WebSocket subscriptions (`eth_subscribe newHeads` + `logs`) are
exposed on `wss://api.curs3d.fr/ws`.

The headline native subset:

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/status` | Chain height, finalized height, epoch, validators, protocol version |
| GET | `/api/healthz` | Health check with block age |
| GET | `/api/metrics` | Prometheus-format metrics |
| GET | `/api/block/:height` | Block with transactions, state root, merkle root |
| GET | `/api/blocks?from=&limit=` | Paginated blocks (max 100) |
| GET | `/api/account/:address` | Balance, nonce, staked balance |
| GET | `/api/account/:address/proof` | Merkle account proof |
| GET | `/api/contract/:addr/storage/:key/proof` | Merkle storage proof |
| GET | `/api/tx/:hash` | Transaction by hash |
| GET | `/api/receipt/:hash` | Transaction receipt with gas details and logs |
| GET | `/api/logs?...` | Filtered log entries (by contract, topic, block range) |
| GET | `/api/pending` | Mempool pending transactions |
| GET | `/api/validators` | Active validator set with stakes |
| GET | `/api/tokens` | List all CUR-20 tokens |
| GET | `/api/token/:address` | CUR-20 token metadata |
| GET | `/api/token/:addr/balance/:owner` | CUR-20 token balance |
| GET | `/api/governance/proposals` | List governance proposals |
| GET | `/api/governance/proposal/:id` | Proposal details |
| POST | `/api/faucet/request` | Request 100 CUR testnet tokens (1h cooldown) |
| POST | `/api/tx/submit` | Submit signed transaction (auth optional) |
| POST | `/api/tx/estimate` | Dry-run: gas estimate, fees, replacement check |
| WS | `/ws` | Real-time events: new_block, new_transaction, finality |

All responses: `{"ok": true, "data": {...}}` or `{"ok": false, "error": "..."}`.

Auth: set `CURS3D_API_TOKEN` env var to require `Authorization: Bearer <token>` on POST endpoints.

## Smart Contracts

CURS3D runs WebAssembly contracts via Wasmer 7 with Cranelift. The VM injects fuel metering per instruction — contracts with unmetered loops are rejected at deploy time.

| Operation | Gas Cost |
|-----------|----------|
| Base transaction | 21,000 |
| Contract deploy | 32,000 |
| Contract call | 2,600 |
| Storage read | 200 |
| Storage write | 5,000 |
| Log emit | 375 |
| Per byte (data) | 16 |
| WASM loop tick | 50 |

**Host functions:** `storage_get`, `storage_set`, `storage_read`, `storage_write_bytes`, `emit_log`, `emit_log_bytes`, `input`, `input_len`, `input_read`, `consume_gas`, `loop_tick`

## Consensus

1. **Slot-leader scheduling (v4)** — `slot_leader(height, validator_set) = SHA-3(height || prev_hash) % cumulative_stake`. Deterministic, stake-weighted, single proposer per height. Block production in `network/mod.rs` is gated on `self.address == slot_leader(next_height, ...)`.
2. **Block Production** — Slot-leader signs the block every 10 seconds
3. **Finality Votes** — Validators sign attestations (`block_hash || height || epoch`)
4. **Finalization** — Block irreversible when votes representing >= 2/3 total stake are collected
5. **Slashing** — Dual-signed block headers at same height = cryptographic proof. 33% stake penalty + 64-block jail
6. **Epochs** — Validator set frozen per epoch (default 32 blocks). No mid-epoch manipulation
7. **Fork Choice** — Heaviest cumulative proposer-stake wins. Finality boundary prevents deep reorgs

## Testing

```bash
RUSTUP_TOOLCHAIN=nightly cargo test --lib                    # 181 tests, all green
RUSTUP_TOOLCHAIN=nightly cargo clippy --lib -- -D warnings   # 0 warnings (CI enforces)
RUSTUP_TOOLCHAIN=nightly cargo fmt --check                   # Enforced formatting
```

Coverage: cryptographic operations, block validation, native + EVM transaction flow, staking/unstaking, slashing with evidence, BFT finality threshold, slot-leader determinism, fork choice, block tree pruning, wallet encryption/decryption, storage persistence, native WASM + revm VM execution, gas metering, state sync snapshots, epoch management.

## Genesis Configuration

```json
{
  "chain_id": "my-testnet",
  "chain_name": "My Testnet",
  "block_reward": 50000000,
  "minimum_stake": 1000000000,
  "unstake_delay_blocks": 10,
  "epoch_length": 32,
  "jail_duration_blocks": 64,
  "block_gas_limit": 10000000,
  "initial_base_fee_per_gas": 0,
  "base_fee_change_denominator": 8,
  "allocations": [
    {
      "public_key": "0xVALIDATOR_PUBLIC_KEY_HEX",
      "balance": 1000000000000,
      "staked_balance": 5000000000
    }
  ],
  "upgrades": [
    { "height": 1000, "version": 2, "description": "Enable feature X" }
  ]
}
```

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Make your changes
4. Ensure `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` all pass
5. Open a Pull Request

All contributions welcome — whether it's code, documentation, bug reports, or ideas.

## License

MIT License. See [LICENSE](LICENSE) for details.
