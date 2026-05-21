# Refactor — `replace_blocks` rewind/replay (reorg path)

**Status**: Design. Depends on #28 Phase B.
**Tracking task**: #34.
**Estimated effort**: 3 days focused (after #28 Phase B lands).
**Blocking**: any forked-chain recovery scenario; mainnet readiness.

## Motivation

`Blockchain::replace_blocks(new_blocks: Vec<Block>)` is the path taken
when the chain reorganizes — typically after a snapshot sync where the
remote tip is higher than ours, or after a fork resolution where a
deeper branch wins fork choice.

Today's implementation (`src/core/chain.rs`) re-applies *every* block
from genesis when called, because it can't reason about which slice of
state needs to be invalidated. On a 10k-block chain this means up to
10k EVM/Wasmer executions on a reorg — minutes of CPU, during which
consensus is wedged.

With the paginated block store (#28) in place, we can do better:
rewind the *common ancestor* and only re-apply the suffix.

## Goal (P0)

`replace_blocks` runtime stays `O(suffix_length)` instead of
`O(chain_length)`. For a 5-block reorg, this means 5 applies, not
10000.

## Pre-conditions

- #28 Phase B done: `Blockchain` reads from `BlockStoreCursor` instead
  of `self.blocks[...]`.
- `BlockStoreCursor::invalidate_from(h)` works (already in #28 Phase A).
- State persistence is reversible at block boundaries. Today it is:
  every block apply writes state via the async-persistence layer with
  versioned keys. We need to add a "rollback to state-at-height-h"
  primitive (Phase 1 below).

## Design

```rust
impl Blockchain {
    pub fn replace_blocks(&mut self, new_blocks: Vec<Block>) -> Result<(), ChainError> {
        // 1. Find the common ancestor.
        let first_new = new_blocks.first().ok_or(ChainError::EmptyReorgInput)?;
        let common_height = first_new.header.height.saturating_sub(1);

        // 2. Reject reorgs that cross the finalized boundary.
        if common_height < self.finalized_height() {
            return Err(ChainError::ReorgCrossesFinality {
                common: common_height,
                finalized: self.finalized_height(),
            });
        }

        // 3. Rewind state to common_height.
        self.rewind_state_to(common_height)?;
        self.cursor.invalidate_from(common_height + 1);

        // 4. Apply the new suffix.
        for block in new_blocks {
            self.add_block(block)?;
        }

        Ok(())
    }

    fn rewind_state_to(&mut self, target_height: u64) -> Result<(), ChainError> {
        // a) Restore account/contract/storage state from snapshot/storage.
        // b) Drop receipts at heights > target_height.
        // c) Reset finality tracker to the latest pre-target vote.
        // d) Restore mempool (txs from reverted blocks go back to pending).
        // e) Adjust base_fee_per_gas to the value at target_height.
        Ok(())
    }
}
```

## Phases

### Phase 1 — Reversible state persistence (1 day)

Today: every apply writes state to redb. A rewind needs the inverse.

Option A — **Periodic state snapshots** (simpler, more disk):
- Take a full state snapshot every N blocks (N=64, one per epoch).
- To rewind to height H, load the closest snapshot ≤ H, then re-apply
  blocks (H_snap+1..=H).
- Tradeoff: disk usage grows linearly with chain length, but rewind is
  always bounded.

Option B — **Per-block undo log** (more complex, less disk):
- For every block apply, record an "undo entry" (set of (key, old_value)
  pairs that were modified).
- Rewind = apply undo entries in reverse.
- Tradeoff: lower disk, but undo logs are themselves load-bearing data
  that must be persisted correctly.

**Recommendation: Option A**. Aligns with epoch boundaries we already
snapshot for finality, simpler invariants, easier to test. Snapshot
size on testnet at h=10k is ~200 MB; we have the disk.

### Phase 2 — `rewind_state_to` impl (1 day)

1. Load the closest snapshot ≤ target.
2. Replay blocks from snapshot+1 to target inclusive (using
   `BlockStoreCursor::block_at`).
3. Reset `finality_tracker.finalized_height` to the last finalized
   value ≤ target (must be persisted alongside snapshots).
4. Repopulate mempool with txs from reverted blocks that are still
   valid (nonce/balance check; some will fail and get dropped).

### Phase 3 — `replace_blocks` rewrite (0.5 day)

Implement the design above. Replace the current "re-apply from
genesis" loop.

### Phase 4 — Tests (0.5 day)

- Unit: reorg of length 1, 5, 50, near a finalized boundary (reject).
- Property: random reorg lengths converge to the right state root.
- Localnet: trigger a fork mid-soak; both nodes converge in <5 s.

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Reorg across finalized boundary corrupts the chain | Catastrophic | Hard reject (already proposed); never bypass even for "admin" reorgs |
| Snapshot loaded from disk is corrupt / partially-written | High | Atomic write via redb commit; reject snapshots whose state_root doesn't match the recorded block.header.state_root |
| Mempool restoration replays already-included txs | Medium | After each restored tx, check `state.nonce >= tx.nonce` → drop if true |
| Reorg of length 0 (degenerate) | Low | Detect and no-op early |

## Acceptance criteria

1. A 5-block reorg on a 10000-block chain completes in <500 ms.
2. State root after reorg matches what would result from a fresh sync.
3. Reverted txs (with valid nonce/balance) reappear in mempool.
4. Reorg crossing finality is rejected with `ChainError::ReorgCrossesFinality`.
5. `replace_blocks` is called by both snapshot sync and fork-choice
   resolution code paths — both must produce identical state.
