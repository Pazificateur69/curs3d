# CURS3D 10/10 Execution Checklist

Date: 2026-05-05
Status: living checklist for recruiter/investor-grade readiness.

## Priority 1 — Stability, CI, Deployment

### Network Stability

- [x] Restart prevention: validators pause production during startup, sync, and peer-mesh settle windows.
- [x] Boot fork prevention: block #1 is primary-leader-only at consensus validation level, so genesis timestamp `0` cannot unlock every backup.
- [x] Late join catch-up: `RequestBlocks` accepts stale-but-contiguous batches and cold-sync tests cover 30 and 100 blocks.
- [x] Fork recovery without wipe: snapshots may replace divergent non-finalized suffixes.
- [x] Finality safety: snapshots still reject divergent finalized checkpoints.
- [x] Temporary no peers: failed gossipsub broadcasts are queued and retried.
- [x] Same-height divergent verified peer: node pauses production and requests snapshot.
- [x] Higher verified peer tip: node pauses production and requests blocks/snapshot before producing.
- [ ] Live soak: 24h, then 72h, with all public validators on same height/hash/finality and no operator wipe.

### Monitoring

- [x] `/api/status` includes height, latest hash, finalized height, active validators, protocol version, peer count, latest block age.
- [x] `/api/metrics` includes Prometheus gauges for height, finalized height, active validators, peer count, accounts, contracts, receipts, logs, base fee.
- [x] Healthcheck cron restarts process/API failures but treats consensus stalls as alert-only by default.
- [x] Discord/Telegram alert hooks documented via `/etc/curs3d/alerts.env`.
- [ ] Public status page must show per-node height/hash/finality/peers/RPC/explorer uptime from live scrapes.
- [ ] Alert routing must be tested on production VPSes.

### CI/CD

- [x] `cargo check`
- [x] `cargo test --lib`
- [x] `cargo clippy --all-targets -- -D warnings`
- [x] `cargo fmt --all --check`
- [x] `cargo audit`
- [x] `forge test`
- [x] Docker build
- [x] static JS/OpenAPI/healthcheck syntax smoke
- [x] local `/api/status` + `/eth` JSON-RPC smoke test

### Reproducible Deployment

- [x] `deploy/scripts/deploy.sh` installs binary, systemd, nginx, healthcheck, backup timer.
- [x] `deploy/DEPLOY_RUNBOOK.md` documents node layout, RPC endpoints, hardfork v5, healthcheck behavior, rollback basics.
- [x] Deploy script path no longer depends on raw `PRIVATE_KEY`; Foundry deploy uses `--keystore --sender`.
- [ ] One command per node for a non-wipe rolling deploy needs a final live rehearsal.
- [ ] Release tags and changelog must be cut after the network stability patch lands on `main`.

## Priority 2 — Realistic Tests, Demo, Docs

### Tests Closer To Reality

- [x] Persistent restart/state-root tests exist for storage, contracts, receipts, epoch boundary replay.
- [x] Two-node cold sync test covers 30 blocks.
- [x] Two-node late join test covers 100 blocks / multi-batch sync.
- [x] Snapshot tests cover non-finalized fork recovery and finalized checkpoint rejection.
- [x] Leader timeout logic is covered by consensus rank tests and height-1 primary-only regression.
- [x] EVM deploy/call/log tests exist at VM level.
- [x] EVM transaction hash/receipt compatibility test covers Ethereum wire hash lookups.
- [x] Foundry tests cover token, faucet, staking, governance, attestations, vault, escrow, fuzz and invariants.
- [ ] Full 3-node local chaos test should be added after the public network stabilizes: restart, partition, heal, late join.

### Public Demo

- [x] Static Solidity portfolio dApp exists under `contracts/dapp`.
- [x] dApp supports wallet connect, faucet/test token flow, attestation creation/read, staking/governance surfaces.
- [x] Public RPC config is documented: `https://rpc.curs3d.fr/eth`, chain ID `1800329576`.
- [ ] Host the Solidity portfolio dApp on the public site with a visible "Try on testnet" entrypoint.
- [ ] Add a 3-minute happy-path demo video or GIF: MetaMask, faucet, mint/attest, explorer receipt.

### Recruiter / Investor Docs

- [x] README positions CURS3D as an experimental post-quantum L1, not a mainnet-ready product.
- [x] Known risks are explicit: no external audit, no PGP key, experimental testnet.
- [x] Foundation docs cover thesis, market, product/protocol, security, fundraising readiness, data room.
- [x] Founder/investor speech exists in `docs/branding/12-founder-investor-speech.md`.
- [ ] Add final screenshots and verified live deployment addresses after the next clean deploy.

## Priority 3 — Market Credibility, Product Quality, Public Proof

- [x] Avoid "Ethereum killer" positioning.
- [x] Position as "experimental post-quantum L1" with EVM-compatible surface and native ML-DSA-87 path.
- [x] Explain the EVM/secp256k1 tradeoff honestly: MetaMask compatibility is intentionally non-PQ, native CURS3D txs are PQ.
- [x] Wallet claims aligned with v5: browser signing is write-capable through WASM ML-DSA-87.
- [x] API docs/OpenAPI aligned with protocol v5, 3 validators, `/eth` rate limit.
- [x] Solidity portfolio has serious tests: unit, fuzz, invariant, reentrancy.
- [ ] Tag a release after 72h soak.
- [ ] Publish changelog and incident log from the stabilization work.
- [ ] Get at least one external Rust/Web3 review before calling the system audit-ready.

## Final 10/10 Gate

CURS3D can be called 10/10 for a solo-student Web3 portfolio when all are true:

- 72h public soak with no wipe and no manual fork recovery.
- Restart all public nodes without divergent hashes.
- Start a late node and observe catch-up without wipe.
- Stop the expected leader and observe backup production after timeout.
- Temporarily isolate a node and observe queued rebroadcast/snapshot recovery.
- CI green on Rust, Foundry, Docker, static docs and local RPC smoke.
- Public dApp works for a third-party user without private instructions.
- README and website stay honest: experimental testnet, no external audit yet, no monetary value.
