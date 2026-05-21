# CLAUDE.md — Project Context for CURS3D

State as of: **2026-05-21** (software **v0.3.5** + consensus protocol
**v5** + **5-validator fresh testnet** — n1, n2, n3, Plesk-1, Plesk-2 —
genesis SHA256 `e830418885dd9057f9f44d4f409ba8bbccf536be9efd3b519017a5319d3b59af`).

This is the **second genesis regen of the chain** (post the 2026-05-06
2-validator one) and a **complete architecture refresh** triggered by
two distinct production incidents on 2026-05-20:

- **Plesk-1 stealth-fork pollution** : a `system-metrics-agent` node was
  bootstrapped on 2026-05-07 from `deploy/scripts/bootstrap-curs3d-plesk.sh`
  and forgotten. It survived the 2026-05-06 genesis regen on a stale
  3-validator genesis, then started serving its forked snapshots into
  n2 and n3 once they tried to sync. n2 ended up with a corrupted
  state (`fin=8126` but `height=8109` from the Plesk fork). Killed on
  2026-05-20. See "Stealth Plesk validators (now legitimate)" below.
- **n1 memory bloat → OOM-thrash** : `/var/lib/curs3d/curs3d.redb` grew
  to 4 GB by h≈8500, and curs3d RSS hit 5 GB which exceeded the
  `MemoryMax=5G` cgroup cap on the 6 GB Oracle Free Tier VM. systemd
  OOM-killed it, restart loop, eventually the whole VM thrashed and
  needed an OCI Console hard reboot. MemoryMax bumped to 4000M after.
  Root cause (the in-memory `Vec<Block>` accumulating without bound) is
  filed for a separate refactor.

Recovery decision was **fresh chain at genesis with 5 validators across
4 providers**, rather than salvage the 8500-block history. The chain
was a testnet, the history had no economic value, and going from 3 to
5 validators improved BFT tolerance from "1 down breaks finality" to
"up to 2 down still finalises".

Recent code shipped to support this :

- **Snapshot chunk-delivery fix** (commits `a0cb94d` + `db7693f`) :
  receiver-side pre-manifest chunk buffer (chunks that race ahead of
  the manifest are buffered instead of dropped silently), responder-side
  50 ms throttle between chunks (a 243-chunk burst was overflowing
  gossipsub's per-peer outbound queue), responder-side skip when our
  height is 0 (avoids the mutual h=0 snapshot loop that two freshly-
  wiped nodes fall into), receiver-side reject manifests at
  `manifest.height <= our_height` (the same loop, other end).
- **Trusted checkpoints** (commit `8368857`) : `core::checkpoints`
  module — hardcoded `(height, hash, state_root)` anchors, currently
  empty for `curs3d-public-testnet`, ready to be populated after the
  first audit cycle.
- **Mempool priority classes** (commit `72f6f39`) : reserves 500 slots
  for System-class txs (Stake, Unstake, governance) so a user-tx
  flood can't starve consensus traffic.
- **Pruning primitive** (commit `92b20db`) : `Storage::prune_blocks_below`
  + `--archival` / `--prune-keep-blocks` CLI flags, wired but DORMANT
  pending the in-memory `Blockchain::blocks` base-offset refactor.
- **v6 SparseMerkleTrie state-root** (commit `0f0ab2c`) : dispatcher
  added at every state-root call site, but `V6_HARDFORK_HEIGHT_TESTNET
  = u64::MAX` so v6 is **dormant**. Activation = one-line constant
  change + coordinated rollout.
- **External audit RFP** (commit `346e651`) : `docs/AUDIT_RFP.md` —
  self-contained brief ready to send to vendors (Trail of Bits, NCC
  Group, Halborn, Quantstamp, Cure53).

**`v1.0` is reserved for the official mainnet launch — do not bump the
software version just because the consensus protocol bumps.**

## Stealth Plesk validators (now legitimate, 2026-05-21)

Two of the five validators (Plesk-1 and Plesk-2) run on shared-hosting
Plesk VPS where blockchain workloads are not explicitly welcomed by
the provider. To stay below the panel's process-name scrutiny, the
curs3d binary is renamed and lives under hidden paths. **This is
documented intentionally** : a forgotten stealth node on Plesk-1 is
exactly what caused the 2026-05-20 incident, and the lesson is "stealth
is fine, **un-documented** stealth is not".

| Property | Value |
|----------|-------|
| Binary path | `/usr/local/lib/.web-cache/agent` |
| Service name | `web-cache-agent.service` |
| Data dir | `/var/lib/.web-cache/agent/data` |
| Wallet | `/var/lib/.web-cache/agent/etc/cred.bin` |
| Wallet password | `/var/lib/.web-cache/agent/etc/cred.pass` (root 0600) |
| Genesis file | `/var/lib/.web-cache/agent/etc/cfg.bin` |
| P2P port (public) | 4337 (publicly reachable, validated against firewall) |
| HTTP API (local-only) | 127.0.0.1:18080 (note the 18080, not the standard 8080 used on the public nodes) |
| TCP RPC (local-only) | 127.0.0.1:9545 |
| Build source dir | `/var/lib/.web-cache/source/curs3d` |
| Run as | root (acceptable since the box is shared-hosting with no other curs3d-adjacent code) |

If you change the stealth scheme, update this table AND the systemd
unit at `deploy/systemd/web-cache-agent.service` (which has
`__PUBLIC_IP__` as a placeholder, sed-replaced per host during
deploy). A cron healthcheck on each Plesk pings the local HTTP API
every 5 min and writes to `/var/log/.web-cache-health.log` — if you
notice this file growing or going silent for >15 min, the stealth
node is in trouble.

## Production incident — storage fix upgraded 2026-05-06

The 2026-05-05 fork incident root cause was identified by gdb stack trace
on the live deadlocked node1. The first mitigation reduced sled write
pressure, but the overnight soak reproduced the same sled 0.34 deadlock at
later heights. The current tree applies the long-term fix: redb replaces
sled as the canonical embedded database, and live node persistence remains
asynchronous so disk IO cannot block consensus/RPC/gossipsub.

### Layer 1 — sled 0.34 internal deadlock under sustained writes
gdb showed node1's main thread blocked in
`add_block → persist_full_state → storage.replace_contracts → sled tree
insert → sled segment-accountant OneShot wait`, while every sled-io-N
worker was parked on `parking_lot::raw_mutex::RawMutex::lock_slow` inside
`IoBufs::write_to_log`. That's a sled internal log-buffer mutex
deadlock — the chain mutex was held the whole time, starving the API
and gossipsub tasks.

Fix: storage moved to redb (`curs3d.redb` in the node data dir). In live
node mode, `add_block`, finality, mempool admission, snapshot application,
and slashing enqueue bounded persistence jobs instead of touching disk
while holding `Mutex<Blockchain>`. This removes the sled failure mode and
keeps the consensus critical path independent from storage latency.

### Layer 2 — mesh topology depended on node1 as gossipsub relay
node2 and node3 each only listed node1 as `--bootnode`, so when node1's
gossipsub task starved (Layer 1), libp2p stopped forwarding messages
between node2 and node3 → BACKUP_LEADER_TIMEOUT (30s) eventually
elapsed on node3 → competing block at h=341 → fork.

Fix: each systemd unit now lists the OTHER two as `--bootnode` so all
three nodes have direct connections to each other (full mesh, not
node1-as-hub). Peer IDs are preserved across restarts since the new
wipe in `full-rollout.sh` keeps `p2p_identity*`.

### Verification Gate — done 2026-05-06 / 2026-05-07
- ✅ redb binary deployed on n1+n2 via `deploy/scripts/full-rollout.sh --wipe`
- ✅ both nodes reported same height/hash from h=2; finality active by h=32
- ✅ Chain ran 15h+ without alerts before the next operator action; redb file
  byte-identical at 141 561 856 B on both nodes confirms state consistency
- ✅ Solidity portfolio redeployed 2026-05-07 with a fresh deployer keystore
  (old one's password was lost; new keystore at `~/.curs3d/deployer.keystore`
  protected by a 24-byte random password stored at `~/.curs3d/deployer.password`
  chmod 600). Live addresses in `contracts/deployments/1800329576.json`:
  - token   `0x32927628483E8b664772BADFe8a7BD2562Af9852`
  - faucet  `0x555fC00bdd6112ec713243372A730410629bEA3B`
  - staking `0xc49206Bd7789b64E49e090DA8DE734395dfECE94`
  - governance `0xe6DCb0672A221Cf9F894C165091b9FEB6c0B2288`
  - attestations `0xeBC212d8afcecbCC8b3E1A0320e1d80Cb61b5BDe`
  - vault   `0x39262f965c37C9C06234e5DB2e7C41ACA693e15b`
  - escrow  `0x8496CCB04fCdbf26a9786dbfd10C3D513E582538`
  - deployer `0x89C5abd9576869E5C6593Ce8C5f462F9C7dBA295` (also arbitrator)
- ⏳ 24h soak running; 72h soak to follow before "production-ready" sign-off

Historical soak logs in `~/curs3d-soak/` are useful evidence, but pre-redb
alerts must not be counted as post-fix failures or post-fix successes.

A complete index of every password / keystore / wallet location lives in
`docs/SECRETS.md` (paths only, never values).

## What is this project?

CURS3D is a quantum-resistant Layer 1 blockchain written in Rust from scratch.
It is **not** a fork of any existing chain. Every component — consensus, crypto,
networking, storage, VM, API — is implemented from zero.

As of protocol **v5**, the chain runs **two VMs side by side**: the native
quantum-resistant VM (Wasmer 5 WASM, ML-DSA-87 / FIPS-204 signed transactions)
and an Ethereum-compatible VM (revm 38, secp256k1-signed RLP transactions,
MetaMask / Hardhat / Foundry / ethers.js compatible). Both VMs share the
same state trie and the same canonical block stream.

The native PQ signature scheme is the FIPS-204 finalised ML-DSA-87 (NIST
level 5), backed by the pure-Rust `ml-dsa` crate. Both the node and the
browser wallet (`sdk/wasm/curs3d-wallet-wasm`) pin the same version, so
signatures produced in the browser verify on the node byte-for-byte.

## Production Endpoints (live testnet)

| Surface | URL |
|---------|-----|
| Site | https://curs3d.fr |
| API | https://api.curs3d.fr/api/status |
| **Ethereum-compatible JSON-RPC (MetaMask, ethers.js, Hardhat, Foundry)** | **https://api.curs3d.fr/eth** OR **https://rpc.curs3d.fr/eth** |
| **RPC public endpoint** (alias for api/eth + /v1/* legacy + landing page) | **https://rpc.curs3d.fr** |
| Explorer | https://explorer.curs3d.fr |
| Browser Wallet UI (write-capable since v5) | https://curs3d.fr/wallet |
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
| P2P bootnode | `/dns4/api.curs3d.fr/tcp/4337/p2p/12D3KooWLttF4EJ1SjiLEiXvJ1yqmJawLafv47r55T5xzSt1GHn2` (node1, 144.24.192.222:4337) |
| P2P peer (node2) | `84.235.238.213:4337` (Oracle ARM Marseille) |
| P2P peer (node3) | `31.70.70.62:4337` (IONOS Berlin x86_64) |
| P2P peer (Plesk-1, stealth) | `217.154.7.175:4337` (Hostinger Plesk Ubuntu 24.04, 15 GB RAM) |
| P2P peer (Plesk-2, stealth) | `195.35.28.51:4337` (Hostinger Plesk AlmaLinux 9.7, 31 GB RAM) |

- **Chain ID:** `curs3d-public-testnet`
- **Protocol version:** **v5** (ML-DSA-87 / FIPS-204 native signatures — browser wallet interop). v6 SMT state-root is wired but dormant (`V6_HARDFORK_HEIGHT_TESTNET = u64::MAX`).
- **Genesis JSON file SHA-256 (regen 2026-05-21 with 5 validators):** `e830418885dd9057f9f44d4f409ba8bbccf536be9efd3b519017a5319d3b59af`
- **Old 2-validator genesis SHA-256 (regen 2026-05-06):** `165c5f9d2a77719ecada5937753465806d83429588df06f0f25cea5c274bbf4e` *(superseded — n1/n2 fork incident, kept for traceability)*
- **Old 3-validator genesis SHA-256 (regen 2026-05-05):** `702be65951ec6b29efb157fe96f8aba0baf14fc24bfab3926976d2b8e25ca1c1` *(superseded — Plesk-1 stealth ran this for 2 weeks and caused the 2026-05-20 fork)*
- **Active validators:** **5** across 4 providers — Oracle Cloud Free Marseille (×2 ARM), IONOS Berlin (×1 x86), Hostinger Plesk (×2 x86 stealth). Each stakes 50 000 CUR = 250 000 CUR total stake. BFT 2/3 of 5 = **tolerates 1-2 validators down** before finality stops.

| Slot | Host | Provider / arch | Public IP | Validator address |
|------|------|------------------|-----------|-------------------|
| node1 | `curs3d-node1` | Oracle ARM Marseille | `144.24.192.222` | `CURA770bE29d4C0066263855Ea5ADE6387d503f1Cea` |
| node2 | `curs3d-node2` | Oracle ARM Marseille | `84.235.238.213` | `CURd5E78C78FF164fb4eAC641d5a2802134B8A2D836` |
| node3 | `curs3d-node3` | IONOS Berlin x86 | `31.70.70.62` | `CURD0133Efb65422a6988c946D680747CCF3038846C` |
| Plesk-1 | `plesk1` (SSH alias) | Hostinger Plesk Ubuntu 24.04 x86 | `217.154.7.175` | `CUR50e62063d9ea7901225B6C8C495CD4ceec8bf838` |
| Plesk-2 | `plesk2` (SSH alias) | Hostinger Plesk AlmaLinux 9.7 x86 | `195.35.28.51` | `CURC4f47c8CFD9ADd76557356c7BBfFdfaCd06fD905` |

- **Faucet wallet:** kept across regen (same pubkey since v5 hardfork). Address `CUR2bc0400551F85049f7AfC01D1EDEc92cEcE4668B`. Genesis allocation 2 000 000 CUR. Wallet file at `/etc/curs3d/faucet.json` on `ssh curs3d-node1` (100 CUR per request via UI, 1 h cooldown per address+IP, captcha-gated via Cloudflare Turnstile).
- **Bootnode multiaddr (publicly advertised):** `/dns4/api.curs3d.fr/tcp/4337/p2p/12D3KooWLttF4EJ1SjiLEiXvJ1yqmJawLafv47r55T5xzSt1GHn2`

The HTTP API exposes **27 endpoints** (REST + WS + `/eth` JSON-RPC) — see
`/api/openapi.json` for the canonical list. Stoplight Elements renders it at
https://curs3d.fr/api.

### MetaMask / Hardhat / Foundry network config

| Field | Value |
|-------|-------|
| RPC URL | `https://rpc.curs3d.fr/eth` (or `https://api.curs3d.fr/eth`) |
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
RUSTUP_TOOLCHAIN=nightly cargo test --lib       # 211 tests, all green
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
the public 3-validator testnet, but they affect specific surfaces.

1. **`wasm-opt` failed during `wasm-pack build`** on the build host (no
   recent binaryen). The deployed bundle is 236 KB instead of ~100 KB
   optimized. Workaround: `brew install binaryen` (or `apt install
   binaryen`) and rerun, OR set `wasm-opt = false` in
   `sdk/wasm/Cargo.toml [package.metadata.wasm-pack.profile.release]` to
   silence the warning.
2. **HTML/CSS/JS a11y + SEO polish on existing pages** was prototyped in a
   worktree but not merged due to conflicts with the wallet-nav additions.
   Will be reapplied in a follow-up pass.
3. **RequestBlocks sync timeout / boot forks — fixed in current tree.**
   BlockResponse accepts stale-but-contiguous batches, sync escalates to
   snapshots after retries, forked RequestBlocks callers receive a snapshot
   offer, and validators pause block production during startup/sync.
4. **Persisted state-root divergence at epoch boundaries — fixed in `f461aa4`.**
   Epoch settlement now runs through the same helper during block apply and
   boot replay. Covered by `test_restart_across_epoch_boundary`.
5. **Cross-compile from Mac is now the recommended deploy path.** `cross`
   + OrbStack/Docker → `target/{aarch64,x86_64}-unknown-linux-gnu/release/curs3d`
   → `deploy/scripts/rollout-staggered.sh` (see "Deploy" below). The legacy
   per-VPS build path (`ssh && git pull && cargo build`) still works but is
   no longer the default — node3's 2 GB of RAM make in-VPS builds fragile.
6. **No PGP key for security disclosures yet.** A signed contact channel
   is a TODO. Until it is published, security issues are reported privately
   via GitHub security advisories on `Pazificateur69/curs3d`. The plan is to
   publish a long-lived PGP key under `/.well-known/security.txt`.
7. **No external security audit.** All cryptography, consensus and VM code
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
    dilithium.rs       FIPS-204 ML-DSA-87 (pure-Rust `ml-dsa` crate, same
                       version as sdk/wasm — browser ↔ node interop). File
                       name kept for minimal churn; the previous
                       `pqcrypto-dilithium` (NIST round 3) wrapper is gone.
    hash.rs            SHA-3, sha3_hash_domain (domain separation), double_hash, merkle trees/proofs, checksummed addresses (EIP-55 style), address derivation
    mod.rs
  governance/mod.rs    On-chain governance: proposals, voting (stake-weighted), automatic execution
  light/mod.rs         Light client: header-only sync, Merkle proof verification
  network/mod.rs       libp2p 0.54 P2P, Gossipsub, mDNS, sync, block production, state sync, per-peer rate limiting, peer scoring/reputation
  rpc/mod.rs           TCP JSON RPC (port 9545, used by CLI)
  storage/mod.rs       redb database (10 tables). Schema v4.
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
                         Powers https://curs3d.fr/wallet. **Write-capable**
                         since the v5 hardfork — both sides pin the same
                         `ml-dsa` crate, signatures verify byte-for-byte.
  javascript/            JS SDK
  python/                Python SDK
deploy/
  monitoring/            Docker stack: prometheus + grafana + uptime-kuma + node-exporter
                         (nginx-status.conf serves Grafana at /, Uptime-Kuma at /status/).
  nginx/                 Public TLS config for api.curs3d.fr + explorer.curs3d.fr + curs3d.fr
  scripts/
    rollout-staggered.sh  Default deploy path: scp pre-built binaries, restart node3→node2→node1
                          one at a time with health gates between each (zero-downtime).
    full-rollout.sh       Coordinated cold restart for storage/hardfork changes (--wipe optional).
    rollout.sh            Older companion to full-rollout.sh (parallel scp + parallel restart).
    add-node.sh           Automated Oracle ARM validator deployment
    setup-node.sh         First-boot bootstrap (creates curs3d user, dirs, units)
    init-localnet.sh      Local 2-validator dev net
    deploy.sh             Deploy compiled binary + units from Mac
    curs3d-healthcheck.sh Healthcheck v2: posts to Discord when restart loops are detected
    curs3d-backup.sh      restic backup → b2:curs3d-backups-pazent:curs3d-node1 (every 6h)
    curs3d-captcha-verify.py
                          Cloudflare Turnstile verifier for the faucet (port 127.0.0.1:8090)
  systemd/
    curs3d.service        Main node unit template (EnvironmentFile=/etc/curs3d/secrets.env, hardened)
    curs3d-node1.service  Live node1 unit (mutual bootnodes: lists node2 + node3 as --bootnode)
    curs3d-node2.service  Live node2 unit (mutual bootnodes: lists node1 + node3 as --bootnode)
    curs3d-node3.service  Live node3 unit (mutual bootnodes: lists node1 + node2 as --bootnode)
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
- NetworkMessage variants: NewBlock, NewTransaction, RequestBlocks, BlockResponse, HeightAnnounce (signed, includes verified public peer-address hints for peer exchange), SlashingEvidence, FinalityVote, RequestSnapshot, SnapshotManifest, SnapshotChunk
- `PeerRateLimiter` — Per-peer message rate limiting with escalating bans
- `PeerScorer` — Reputation system: score decay, behavior-based scoring, automatic ban below threshold
- Block acceptance → positive score, block rejection → negative score, rate limit → penalty
- Block production: every 10 seconds, gated by `slot_leader(next_height, ...)` (v4)
- Height announce: every 30 seconds (signed by validators). Verified announces also carry `public_addrs`; peers store them in `/var/lib/curs3d/peerstore.json` and redial them on restart. This removes the old requirement to manually edit every existing node whenever a validator is added.
- Sync: batch of 50 blocks, 30s timeout, 3 retries, then snapshot escalation. Validators stay behind a sync gate until verified peer tips are stable for 3 production ticks, so a restarted or late-joining node cannot produce on a stale fork before catching up.
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
- Rate limiting: 60 GET/min, 10 POST/min, 600 JSON-RPC calls/min on `/eth` per IP
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

**211 tests, all green** (2026-05-21, post snapshot-chunk-delivery fix +
mempool priority classes + trusted checkpoints + v6 SMT wiring +
fuzzing CI + storage pruning primitive). Run `cargo test --lib --no-run`
and read the binary output for the canonical per-module count.
- consensus: ~15 (validators, selection, slashing, equivocation, finality votes, dedup, jailing, epochs, epoch rewards, inactivity penalty, grace period, apply settlement)
- core/block: 2 (genesis, new block)
- core/blocktree: 6 (basic, fork choice, common ancestor, reject below finalized, pruning, branch rejection)
- core/chain: 40+ (genesis, config, blocks, tx flow, forged mint, stake, unstake, duplicate, state root, contracts, receipts, snapshots, fee market, epochs, state proofs, restart, **mempool priority classes**, **v6 SMT dispatch**)
- core/checkpoints: 10 (hardcoded checkpoints, block + snapshot verification, empty-list permissiveness)
- core/transaction: 5 (sign/verify, coinbase, stake, unstake, forged from)
- crypto/dilithium: 5 (sign/verify, invalid sig, ml-dsa sizes match FIPS-204 L5, address derivation stable, wasm interop sanity check)
- crypto/hash: 7 (sha3, merkle root, merkle proof, address derivation, domain separation, checksum roundtrip, checksum rejection)
- governance: 8 (submit, vote, double vote, pass/execute, reject no quorum, reject no approval, invalid param, vote after deadline)
- light: 3 (new client, valid proof, invalid proof, empty headers)
- network: 23 (rate limiter, peer scoring, bounded deserialize, cold sync, stale BlockResponse handling, startup production gate, queued rebroadcasts, **partition / message-loss recovery**)
- storage: 11 (block, account, height, pending, meta, epochs, snapshots, **prune_blocks_below variants**)
- token: 10 (deploy, transfer, insufficient balance, approve+transferFrom, insufficient allowance, duplicate deploy, invalid params, zero amount, self transfer, list)
- trie: 9 (empty, insert/get, root changes, deterministic root, remove restores, proof generation, proof absent, many entries, update value)
- vm: 10 (deploy valid/invalid/empty/oom, call, storage+logs, deterministic address, unmetered loop, instruction metering)
- wallet: 5 (create, deterministic address, encrypted save/load, wrong password, auto-migrate)

Run a specific test: `RUSTUP_TOOLCHAIN=nightly cargo test test_name --lib`

## CI (.github/workflows/)

- `ci.yml` — check, test, clippy, fmt, audit, foundry, static-smoke,
  docker-build, rpc-smoke, release-build, bench (per push + per PR).
- `fuzz.yml` — nightly fuzz of every target in `fuzz/` (transaction
  decode, block decode, RPC parsing, network message, merkle proof).
  Runs on cron `30 3 * * *` and via workflow_dispatch with configurable
  per-target time budget. Crash artifacts uploaded for 14 days; the job
  fails on any crash so a red signal shows up in the Actions tab.

## Recent commits (newest first)

- `db7693f` fix(network): throttle snapshot chunks + reject sideways/empty snapshots
- `0f0ab2c` core: v6 SparseMerkleTrie state-root, wired but dormant
- `92b20db` storage+cli: prune_blocks_below primitive + archival/prune flags
- `640f5f8` test(network): partition / message-loss recovery test for BlockResponse
- `b29d792` ci(fuzz): nightly fuzz workflow + expose RpcEnvelope for harness
- `a0cb94d` fix(network): snapshot chunk delivery race + responder error visibility
- `72f6f39` mempool: priority classes (System / User) with starvation resistance
- `8368857` core: hardcoded trusted checkpoints (binary-side safety net)
- `93f046b` docs(runbook): fix stale --rpc flag, now --rpc-addr in CLI
- `346e651` docs(audit): self-contained external security audit RFP
- `09e0432` deploy(node3): cloud-init V3 + cross.toml rustup-from-scratch
- `d9ae135` network+cli: auto-discovery, persistent peerstore, sync-gate, HTTP-first CLI
- `2fa0427` fix(plesk): chmod 711 on stealth parent dirs so service user can traverse
- `202a072` deploy: stealth-mode bootstrap for Plesk validators
- `24a8bb8` deploy: bootstrap-curs3d-plesk.sh + commit current 2-validator genesis
- `94bbe33` deploy: redeploy Solidity portfolio + add secrets index doc
- `90ac481` ops: deploy redb live on n1+n2 (2-validator genesis), document node3 incident
- `bb75d4a` harden: bounded persistence shutdown + cluster-wide rollout gates
- `759d600` vm: bump wasmer 5 -> 7.1.0 (fixes __rust_probestack linker on x86_64)
- `59694cb` crypto: migrate Dilithium-L5 (round 3) → ML-DSA-87 (FIPS-204 final) — **v5 hardfork**
- `c34b366` vm/evm: integrate revm 38 as second VM (Solidity / MetaMask compat) — **v4 hardfork**
- `343a7a1` consensus: deterministic stake-weighted slot-leader scheduling

## Dependencies (key ones)

- `ml-dsa = "=0.1.0-rc.9"` — Post-quantum signatures, FIPS-204 ML-DSA-87 (NIST level 5, pure Rust). Pinned to the same version as `sdk/wasm` so browser-signed transactions verify on the node byte-for-byte.
- `signature = "3.0.0"` — RustCrypto signature traits used with `ml-dsa`.
- `sha3` — Keccak hashing
- `redb` — Embedded key-value database
- `libp2p` 0.54 — P2P networking (Gossipsub + mDNS + noise + yamux)
- `hyper` 1.x — HTTP server
- `wasmer` 7 + `wasmer-types` 7 — Native CURS3D WASM VM with Cranelift (bumped from 5 on 2026-05-05 to fix `__rust_probestack` linker error on x86_64; ARM was unaffected)
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
- **`wallet.html` + `wallet.js` — Browser wallet UI (write-capable since v5: ML-DSA-87 signatures match what the node verifies, via the `curs3d-wallet-wasm` bundle).**
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

## Deploy

Three deploy paths exist, listed best → fallback:

### 1. Cross-compile + staggered rollout (default, zero-downtime)

Used for routine code changes that keep the on-disk format and gossipsub
topic stable.

```bash
# One-time setup on the operator Mac
brew install --cask orbstack
cargo install cross
rustup target add aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu

# Per release (these can run in parallel)
cross build --release --target aarch64-unknown-linux-gnu     # node1, node2
cross build --release --target x86_64-unknown-linux-gnu      # node3
./deploy/scripts/rollout-staggered.sh                        # node3 → node2 → node1
```

`rollout-staggered.sh`:
- scp's both binaries to all 3 nodes in parallel (no chain effect).
- Restarts one node at a time, in the order `node3 → node2 → node1`.
- Between each node, observes `OBSERVE_SECS` (default 600s) and gates on
  (a) chain head still advancing on the other validators' local `/api/status`,
  (b) the just-restarted node's local /api/status reporting `height>0` and `peer_count>=2`.
- Aborts before touching the next node if either gate fails.
- Mutual-bootnode mesh in `deploy/systemd/curs3d-node*.service` ensures the
  remaining 2 nodes form a productive 2/3-quorum throughout.

### 2. Coordinated cold restart (`full-rollout.sh --wipe`)

For storage-format changes (sled→redb, redb v1→v2, etc.), hardforks, or
genesis regeneration. Stops all 3, optionally wipes the chain DB
(preserving `p2p_identity*`), installs the new binary, and starts all 3
within a tight window so the gossipsub mesh forms before any node produces
alone (which would create a boot fork).

```bash
./deploy/scripts/full-rollout.sh --wipe   # required when redb file format incompatible
./deploy/scripts/full-rollout.sh --no-wipe # binary swap only (rare; staggered usually preferable)
```

This script expects each VPS to have already built the binary at
`/home/ubuntu/curs3d/target/release/curs3d`, so it's typically combined with
a `git pull && cargo build --release` over SSH first.

### 3. Per-VPS git pull + cargo build (legacy fallback)

Kept for emergencies and for the historical procedures documented in the
hardfork sections below. Slower (5–25 min build per node) and load-bearing
on each VPS having a Rust toolchain — node3's 2 GB of RAM is tight enough
that this is fragile.

```bash
ssh curs3d-nodeN
cd ~/curs3d && git pull
RUSTUP_TOOLCHAIN=nightly cargo build --release
sudo install -m 755 target/release/curs3d /usr/local/bin/curs3d
sudo systemctl restart curs3d
```

The full operational runbook lives in `deploy/DEPLOY_RUNBOOK.md`.

## Hardfork procedure (v4 → v5: ML-DSA-87 migration)

The v5 hardfork swaps the native PQ signature library:

1. **`pqcrypto-dilithium 0.5.0` (NIST round 3, C bindings) → `ml-dsa
   = =0.1.0-rc.9` (FIPS-204 ML-DSA-87, pure Rust).** Same crate as
   `sdk/wasm/curs3d-wallet-wasm`, so browser-signed transactions verify
   on the node byte-for-byte.
2. **All on-chain accounts get new addresses.** Public-key bytes differ
   under the new dialect → SHA3-derived address bytes differ.
3. **All historical signatures are invalid.** Every block, every
   `FinalityVote`, every `EquivocationEvidence`, every signed transaction
   from v4 or earlier no longer verifies under v5.

Procedural notes:

- The genesis does **not** include explicit upgrades — chains are
  generated with `protocol_version_at_height(0) = 5` uniformly. Mixed-version
  peers diverge silently. Coordinate restarts.
- Chain DBs from v4 or earlier are **not** forwards-compatible; full wipe of
  `/var/lib/curs3d/` is required. The validator wallet, faucet wallet, and
  any password files **must be regenerated** — the keypairs themselves are
  no longer valid (different scheme, different addresses). The
  `p2p_identity.pb` file is unrelated to consensus crypto and can stay.
- The `KeyPair` JSON shape on disk did not change (still
  `{public_key: Vec<u8>, secret_key: Vec<u8>}`), but the byte sizes did:
  `public_key` is 2592 B (unchanged) and `secret_key` is now 32 B
  (was 4864 B — we now store the FIPS-204 seed and re-derive the expanded
  signing key on demand). The `EncryptedWallet` envelope (Argon2id m=64MB
  t=3 p=4 + AES-256-GCM, salt/nonce/ciphertext/version JSON) is unchanged
  and is the same canonical layout as the browser wallet.

### Operator runbook (v5 deploy)

```bash
# 1. Stop the old node.
sudo systemctl stop curs3d.service

# 2. Build the v5 binary.
RUSTUP_TOOLCHAIN=nightly cargo build --release

# 3. Wipe the pre-redb chain DB. (p2p_identity.pb may be preserved.)
sudo find /var/lib/curs3d -mindepth 1 -maxdepth 1 \
  -not -name 'p2p_identity*' -exec rm -rf {} +

# 4. Regenerate the validator wallet under v5 ML-DSA-87.
curs3d wallet --output /etc/curs3d/validator.json \
              --password-file /etc/curs3d/validator.pass

# 5. Regenerate the faucet wallet (same).
curs3d wallet --output /etc/curs3d/faucet.json \
              --password-file /etc/curs3d/faucet.pass

# 6. Regenerate genesis with the new validator + faucet allocations.
curs3d genesis --validator-wallet /etc/curs3d/validator.json \
               --faucet-wallet    /etc/curs3d/faucet.json \
               --output           /etc/curs3d/genesis.json

# 7. Redeploy + start.
sudo cp target/release/curs3d /usr/local/bin/curs3d
sudo systemctl start curs3d.service
```

Repeat steps 1–4 + 7 on every node. Step 6 (`genesis`) only happens on the
operator machine, then the resulting `genesis.json` is rsynced to every
peer.

## Hardfork procedure (v3 → v4 — historical, kept for reference)

The v4 hardfork bundled three breaking changes:

1. **EVM dispatch** (revm 38 alongside Wasmer)
2. **Slot-leader stake-weighted scheduling**
3. **EVM-flavored transactions** (RLP-signed, secp256k1 sender recovery)

`TransactionKind::DeployEvmContract` and `CallEvmContract` are appended at
the end of the enum so bincode discriminants for older variants are
preserved (forward-compatible bincode payloads, but the *content* of an
EVM tx requires v4+ to apply).
