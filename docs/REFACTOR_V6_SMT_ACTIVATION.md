# Refactor — v6 SparseMerkleTrie activation runbook

**Status**: Design / activation plan. Code already wired (commit `0f0ab2c`).
**Tracking task**: #37.
**Estimated effort**: 2-3 weeks (1 day code; 2 weeks localnet soak).
**Blocking**: mainnet (state proofs need SMT root, not v5 merkle root).

## Current state

The v6 protocol upgrade replaces the v5 state-root algorithm (Merkle
tree over `(address, AccountState)` leaves) with a Sparse Merkle Trie
(`src/trie/mod.rs`) whose root is computed from incremental updates
keyed by `H(address)` instead of address-sorted leaves.

**Already done** (commit `0f0ab2c`):
- `compute_state_root_at_protocol(protocol_version, accounts, contracts)`
  dispatcher exists in `core/chain.rs`.
- `compute_state_root_v6_smt(accounts, contracts)` implementation
  exists and is unit-tested.
- `V6_PROTOCOL_VERSION = 6` constant declared.
- `V6_HARDFORK_HEIGHT_TESTNET = u64::MAX` — dormant.

**Still needed**:
- Soak v5 vs v6 in parallel on a localnet to confirm SMT roots are
  deterministic across validators and that compute time is acceptable.
- Calibrate the activation height to give validators a clean rolling-
  restart window.
- Activate.

## Why SMT over Merkle?

| Property | v5 Merkle | v6 SMT |
|----------|-----------|--------|
| State-root recompute on 1 account change | O(n log n) — re-sort all leaves | O(log n) — incremental update |
| Light-client proof size | O(log n) | O(log n) (256 hashes worst-case) |
| Proof size for non-existence | Not supported | O(log n) — sibling-empty marker |
| Cross-chain bridges (Ethereum-compatible) | Awkward | Standard — bridges expect SMT |
| Determinism across implementations | Sensitive to sort order | Fixed by tree structure |

Mainnet wants non-existence proofs (for state proofs sent to light
clients) and bridge interop — both require SMT.

## Pre-activation checklist

### Code (already done)
- [x] `src/trie/mod.rs` SparseMerkleTrie implementation
- [x] `compute_state_root_v6_smt()` unit tests pass
- [x] Dispatcher `compute_state_root_at_protocol()` wired at every
      state-root call site (5 locations as of `0f0ab2c`)
- [x] `protocol_version_at_height()` returns 6 above
      V6_HARDFORK_HEIGHT_TESTNET

### Soak (TODO)
- [ ] Spin up a 2-node localnet with `V6_HARDFORK_HEIGHT_TESTNET=100`
- [ ] Generate transaction load for 200 blocks
- [ ] Confirm both nodes agree on every block's state_root through the
      transition (especially at h=99 → h=100, the v5 → v6 boundary)
- [ ] Restart one node mid-flight, confirm boot replay produces the
      same state_root at every replayed block
- [ ] Measure SMT compute time at h=500, 5000, 50000 — must be under
      100 ms per block (production interval is 10 s; SMT shouldn't be
      the bottleneck)

### Spec + comms (TODO)
- [ ] Update `SPEC_CONSENSUS.md` to describe v6 fully
- [ ] Update `whitepaper.md` Sec.X to mention v6 transition
- [ ] Pre-activation announce on Discord / X / GitHub: "v6 hardfork
      activates at height N on date D — validators must run v0.4.0+ by
      then"

## Activation procedure

### Day 0 (announce)
- Choose `V6_HARDFORK_HEIGHT_TESTNET` = `current_head + 2880` (~8 h
  ahead at 10 s blocks). Gives 8 h for validators to update.
- Commit the constant change.
- Build the binary.
- Announce to all validator operators (Discord + email).

### Day 0 + 8 h (deploy)
- Each validator runs `deploy/scripts/rollout-staggered.sh`.
- Verify all 5 are on the new binary before the activation height.

### At activation height
- The first block with `header.height == V6_HARDFORK_HEIGHT_TESTNET`
  uses the v6 SMT root. Every validator agrees because we all upgraded.
- A validator still on old binary will reject the block as "invalid
  state root" and stop producing. (Hardfork by design.)

### Day +1 (verify)
- Check finality continues uninterrupted.
- Check that the new state_root at h=activation matches what the SMT
  produces for the post-apply state.
- Check that `/api/state-proof/:address` returns SMT-shaped proofs.

### Rollback plan (kill-switch)
If something goes catastrophically wrong in the first hour post-
activation:
1. Stop all validators.
2. Revert `V6_HARDFORK_HEIGHT_TESTNET` to `u64::MAX`.
3. Wipe state below activation, restart from genesis.
4. Coordinate operator announcement.

The hardfork is recoverable because we're still on testnet — no
economic value lost.

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| SMT root non-deterministic across validators (e.g. iteration order matters) | Catastrophic | Soak before activation; if even one block has mismatched roots, abort |
| SMT compute time grows super-linearly with state size | Medium | Measure at h=50k localnet before activation; cap fix is a memoization layer (out of scope) |
| Mixed-version peers diverge silently for one block | High | Activation block validation is strict; rejects propagate quickly so split is detected |
| Light-client proof format breaking external integrations | Medium | We have no external light-client consumers yet; document the new format and announce in advance |
| State_root mismatch persists indefinitely if rollback delayed | High | Operator runbook: "if state_root mismatch detected within 60 s of activation, hit kill-switch" |

## Acceptance criteria (post-activation)

1. All 5 validators agree on `state_root` for every block at and after
   activation.
2. `/api/state-proof/:address` returns SMT-style proofs.
3. Finality lag stays < 5 blocks for at least 24 h post-activation.
4. No mismatched-root warnings in Grafana.
5. `compute_state_root` p99 latency < 100 ms.

## Out-of-scope

- Storage proofs (for contract slots) — handled separately as an
  extension of the SMT proof format.
- Pruning SMT internal nodes — separate refactor.
- Bridge integration — requires another chain to consume the proofs.
