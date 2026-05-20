# CURS3D External Security Audit — Request for Proposal (RFP)

**Status:** Draft v1, 2026-05-20.
**Owner:** Pazent (agencenetstrategy@gmail.com, github.com/Pazificateur69).
**Scope window:** target start Q3 2026, deliverable before mainnet launch.

This document is the brief to send to external security firms. It is
self-contained: a vendor can scope and price from this without a kickoff
call.

---

## 1. Context

**CURS3D** is a quantum-resistant Layer 1 blockchain written from scratch
in Rust (edition 2024, ~31 kLOC, 181 lib tests). It is **not** a fork of
any existing chain — consensus, crypto, networking, storage, VM and APIs
are all implemented in-house.

The chain runs **two VMs side by side**, sharing the same state trie:
- a native VM (Wasmer 7 + Cranelift) with **ML-DSA-87 (FIPS-204, NIST
  level 5)** post-quantum signatures via the pure-Rust `ml-dsa` crate,
  and
- an EVM (revm 38) with secp256k1-signed RLP transactions for MetaMask /
  Hardhat / Foundry / ethers.js compatibility.

The chain is currently live on a **2-validator public testnet** (genesis
SHA-256 `165c5f9d2a77719ecada5937753465806d83429588df06f0f25cea5c274bbf4e`,
regenerated 2026-05-06). The audit is a **prerequisite to mainnet**, not
to the public testnet.

Repo: <https://github.com/Pazificateur69/curs3d> (will be granted
read+issue access for the audit window).
Internal architecture brief: `CLAUDE.md` at the repo root.
Recent operational incidents and fixes: `CLAUDE.md` "Production incident"
section.

---

## 2. In scope

The audit covers the **Rust node** (`src/`) and the **browser wallet
crypto bundle** (`sdk/wasm/curs3d-wallet-wasm/`). Approximate per-area
priority:

### 2.1 Consensus (highest priority)
- BFT PoS, 2/3 finality, validator set epoch snapshots
  (`src/consensus/mod.rs`).
- Slot-leader scheduling — deterministic, stake-weighted via
  `sha3(height || prev_hash)` modulo cumulative stake.
- Fork choice (heaviest cumulative proposer-stake) and reorg-under-finality
  block.
- Equivocation slashing (33 % stake penalty + 64-block jail), inactivity
  penalties with grace period, epoch rewards distribution.
- Network-level liveness: `BACKUP_LEADER_TIMEOUT = 30s` (raised from 12s
  after a 2026-05-05 parallel-fork incident at h=270 — confirm 30s is
  enough headroom against gossipsub stalls).

### 2.2 Cryptography
- ML-DSA-87 integration (`src/crypto/dilithium.rs`) — verify domain
  separation, message-hashing scheme, signature serialisation, address
  derivation `SHA3(public_key)[0..20]`.
- Browser wallet (`sdk/wasm/`) — same `ml-dsa = =0.1.0-rc.9` pinned both
  sides. Verify byte-for-byte interop, no key-material leakage to the JS
  layer.
- Wallet at-rest encryption: AES-256-GCM + Argon2id (m=64MB, t=3, p=4),
  auto-migration path from legacy plaintext.
- Hashing: SHA-3 Keccak-256 everywhere, double-hash for blocks
  (`sha3(sha3(bincode(header)))`), checksum-encoded addresses (EIP-55).
- EVM side: secp256k1 sender recovery from RLP, gas accounting.

### 2.3 Virtual machines
- Native WASM VM: Wasmer 7 + Cranelift, fuel-metering middleware,
  unmetered-loop rejection at deploy time, 11 host functions
  (storage_get, storage_set, emit_log, consume_gas, etc.).
  Verify: gas accounting cannot underflow, host functions cannot escape
  the contract sandbox, memory pages bounded.
- EVM: revm 38 integration (`src/vm/evm.rs`, ~1066 LOC). Verify state-trie
  sharing with the native VM does not allow cross-VM state corruption,
  EVM gas charging matches Ethereum mainnet exactly where claimed.

### 2.4 Networking
- libp2p 0.54: Gossipsub + mDNS + TCP/noise/yamux, port 4337.
- NetworkMessage decoder — bounded deserialise, per-peer rate limiter,
  peer scorer / ban list, signed HeightAnnounce with verified
  public-address hints.
- State sync via Merkle chunks (`SnapshotManifest` + `SnapshotChunk`).
- Sync gate: validators stay behind sync until verified peer tips are
  stable for 3 production ticks — verify a malicious peer cannot stall
  the gate indefinitely.

### 2.5 Storage and state
- redb 10-table schema (migrated from sled 0.34 on 2026-05-06 after sled
  internal log-buffer mutex deadlock — verify redb usage is similarly
  immune to sustained-write deadlocks).
- Asynchronous live-node persistence: bounded `sync_channel` job queue,
  single-buffered "latest-wins" slot for `FullState`, worker on a
  dedicated thread with `catch_unwind` isolation and graceful `Drop`
  join. Verify: cannot drop a finalised state, cannot reorder
  finality-affecting writes.
- State-root computation, account proofs and storage proofs.
- `SparseMerkleTrie` module is present but **not yet branched to the
  state root** (planned v5 → v6 hardfork). Audit the trie module so the
  upcoming migration does not require a second audit pass.

### 2.6 HTTP and RPC surfaces
- 27 endpoints in `src/api/mod.rs` (REST + WebSocket).
- Ethereum-compatible JSON-RPC at `POST /eth`
  (`src/api/eth_rpc.rs`) — `eth_sendRawTransaction` recovers secp256k1
  sender from RLP and dispatches to revm.
- Per-IP rate limiting (60 GET/min, 10 POST/min, 600 JSON-RPC/min).
- Faucet (`POST /api/faucet/request`) gated by Cloudflare Turnstile via
  nginx `auth_request` → `curs3d-captcha.service`. Constant-time
  comparison of the shared secret header.
- TCP RPC on port 9545 (CLI).

### 2.7 Operational surfaces (lower priority but in scope)
- systemd unit hardening (`deploy/systemd/curs3d-node*.service`).
- nginx vhosts (`deploy/nginx/`).
- restic backups to Backblaze B2 every 6 h.

---

## 3. Explicitly out of scope

- The static website (`website/`) beyond the browser wallet bundle.
- The non-Rust SDKs (`sdk/javascript/`, `sdk/python/`) — these are thin
  client wrappers over the documented HTTP API.
- Discord / Github Actions / Cloudflare configuration. Operational
  surface, not protocol.
- Solidity contracts deployed on testnet
  (`contracts/deployments/1800329576.json` — token, faucet, staking,
  governance, attestations, vault, escrow). Standard OpenZeppelin
  patterns, separate audit if needed later.
- Performance / DoS tuning beyond correctness. Throughput optimisation
  is not part of this engagement.

---

## 4. Deliverables expected

1. **Final written report** (PDF + Markdown), structured per OWASP /
   Trail-of-Bits style: per-finding severity (Informational / Low /
   Medium / High / Critical), reproduction steps, impact, recommendation.
2. **Executive summary** suitable for public release (1-2 pages).
3. **Live debrief call** at end of engagement.
4. **One re-test pass** included in scope: we fix the findings, you
   verify the fixes within 4 weeks of the original report.
5. Public attestation we can publish at `https://curs3d.fr/security` and
   reference in `/.well-known/security.txt` after the re-test pass.

---

## 5. Tentative timeline

| Phase | Window |
|-------|--------|
| Vendor selection + contract | 2026-Q3 (4-6 weeks) |
| Audit engagement | 4-8 weeks (vendor-dependent) |
| Fix window (our side) | 2-4 weeks |
| Re-test pass | 1-2 weeks |
| Mainnet launch | Earliest 2027-Q1 |

We are flexible on the start date to fit the vendor's queue. We are
**not flexible** on shipping mainnet before the re-test pass closes
remediation.

---

## 6. Shortlist (vendors to send this brief to)

In rough order of preference for our profile (PQ crypto + new L1 + dual
VM + small team):

1. **Trail of Bits** — strongest on novel crypto and consensus, public
   reports on Solana, Algorand, Ethereum 2.0.
2. **NCC Group / Cryptography Services** — heavy on PQ specifically;
   they did Falcon and Dilithium reviews for NIST competitors.
3. **Halborn** — chain-native, fast turnaround, good on EVM integrations.
4. **Quantstamp** — broad L1 coverage, good fit if the others are booked.
5. **Cure53** — strong on the browser-wallet side (WASM, JS shim, CSP).

For PQ crypto specifically, also worth a scoping conversation:
- **Kudelski Security** (PQ practice).
- **CryptoExperts** (NIST PQ co-authors).

---

## 7. Budget envelope

Indicative range based on comparable L1 audits in 2025-2026:
**USD 80 k – 180 k** for the scope above, plus the re-test pass.

A separate, smaller engagement (USD 20-40 k) for the browser wallet WASM
bundle alone is acceptable if a vendor wants to split.

---

## 8. Materials we provide on kickoff

- Read + issue access to `Pazificateur69/curs3d` (GitHub).
- Repo internal brief: `CLAUDE.md` (architecture, conventions, known
  bugs, hardfork history).
- Test suite: `cargo test --lib` (181 tests, all green at HEAD).
- Operational runbook: `deploy/DEPLOY_RUNBOOK.md`.
- Incident postmortems: `CLAUDE.md` "Production incident" section
  (sled deadlock 2026-05-05, ssh-lockout 2026-05-06).
- Threat model draft: `website/security.html`.
- Live testnet endpoints (read-only): see `CLAUDE.md` table.

---

## 9. Coordinated disclosure

Findings during the engagement are confidential until the public
attestation goes live. A **CVE-style embargo** of up to 90 days from the
fix-ship date is acceptable for High / Critical findings if the vendor
wants to publish a write-up first.

We do not have a PGP key for security disclosures yet — vendor's PGP key
will be used for the engagement, and we will publish ours at
`/.well-known/security.txt` after the engagement closes.

---

## 10. Contact

- Email: `agencenetstrategy@gmail.com`
- GitHub: `@Pazificateur69`
- Site: <https://curs3d.fr>

Send the proposal as a PDF to the email above. Replies within 5 business
days are appreciated but not required.

---

*This RFP lives in the repo at `docs/AUDIT_RFP.md`. Update there, do not
fork.*
