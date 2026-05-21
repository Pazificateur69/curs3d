# Refactor roadmap — index

Detailed plans for the next 12-24 months of CURS3D engineering, in
priority order. Each refactor has its own design doc with phases,
risks, and acceptance criteria.

## Sprint 3 — Memory bound (THE blocker for mainnet)
- **[#28 Refactor `Vec<Block>` → paginated `BlockStoreCursor`](REFACTOR_BLOCK_STORE.md)** — 1 week
  - Phase A (scaffolding) ✅ done 2026-05-21
  - Phase B-E pending

## Sprint 4 — Auditability + testability
- **[#29 Split `chain.rs` (6846 LOC) into 8-10 submodules](REFACTOR_SPLIT_CHAIN.md)** — 1 week
- **[#30 Split `network/mod.rs::run_with_chain()` handlers](#)** — 1 week (no doc yet)
- **[#31 Trait `PersistenceBackend` (testable / mockable storage)](REFACTOR_STORAGE_TRAIT.md)** — 3 days

## Sprint 5 — Test depth
- **[#32 Chaos localnet 5-7 nodes nightly CI](#)** — 2-3 weeks (no doc yet)
- **[#33 Property-based tests (proptest)](#)** ✅ done 2026-05-21 (13 properties live)
- **[#34 `replace_blocks` O(suffix) reorg rewind](REFACTOR_REPLACE_BLOCKS.md)** — 3 days (depends on #28 Phase B)

## Sprint 6 — Consensus hardening
- **[#35 VRF slot-leader (replace deterministic sha3)](REFACTOR_VRF_SLOT_LEADER.md)** — 2 weeks
- **[#36 Gossipsub peer scoring + custom config](REFACTOR_GOSSIPSUB_SCORING.md)** — 1 week
- **[#37 v6 SparseMerkleTrie state-root activation](REFACTOR_V6_SMT_ACTIVATION.md)** — 2-3 weeks (incl. soak)

## Sprint 7 — Quality + deps
- **#38 Newtypes** ✅ done 2026-05-21 (Address/BlockHash/TxHash with proptest)
- **#39 RPC batching** ✅ done (already implemented in api/eth_rpc.rs:875)
- **#40 WebSocket backpressure** ✅ done 2026-05-21 (5 s write timeout)
- **#41 Bump deps (libp2p 0.54→0.56, revm 38→41, bincode 1→2)** — 1 week, deferred until Sprint 4-5 lands
- **#45 thiserror everywhere** ✅ already done (verified 2026-05-21)
- **#46 Prometheus metrics expansion** ✅ done 2026-05-21 (+6 metrics)
- **#47 tracing crate** ✅ done 2026-05-21 (last eprintln migrated)
- **#48 Mempool eviction LRU + price floor** ✅ done (commit `72f6f39`)
- **#49 Wallet recovery + backup tests** ✅ done 2026-05-21 (+3 tests)

## Out-of-scope until business case
- **#41 deps bumps** — only when forced by RUSTSEC advisories
- **Multi-backend storage (sled, S3)** — premature without users
- **Snapshot deltas** — premature without bandwidth bottleneck
- **Bridge integration** — needs another chain to consume

## Mainnet-readiness gating

Mainnet ships when:

1. **#28** done (Vec<Block> bounded, validators don't OOM)
2. **#29 + #30** done (god modules split, auditable)
3. **#31** done (Storage abstracted, testable)
4. **#32** done (chaos CI passing nightly)
5. **#35** done (VRF activated)
6. **#37** done (v6 SMT activated)
7. External audit complete (Trail of Bits / NCC / Halborn) — see
   [`AUDIT_RFP.md`](AUDIT_RFP.md)
8. 2-3 protocol engineers hired (bus factor > 1)
9. Bug bounty program live with realistic pot

Estimated mainnet horizon: **18-24 months from 2026-05-21** if
above-water, **24-36 months** if solo. See
[`council-log.md`](council-log.md) for the consolidated reasoning.
