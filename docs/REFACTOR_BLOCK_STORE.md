# Refactor — `Vec<Block>` → paginated `BlockStoreCursor`

**Status**: Design / scoping. Implementation gated behind this doc.
**Tracking task**: #28 (internal sprint board).
**Estimated effort**: 1 week focused solo dev + 2 days localnet soak.
**Blocking**: mainnet readiness, validator OOM-resistance.
**Author**: 2026-05-21.

## Motivation

`Blockchain::blocks: Vec<Block>` in [`src/core/chain.rs:313`](../src/core/chain.rs#L313)
holds every block since genesis in RAM. The 2026-05-20 incident on
node1 was a direct consequence: at height ~8500 the in-memory chain hit
4 GB RSS, systemd `MemoryMax=5G` killed the process, restart loop, VM
thrashed, Console reset required.

All three AI advisors (Claude / Gemini / Codex, see
[`docs/council-log.md`](council-log.md)) ranked this **the #1 blocker
for mainnet**. Without it, no validator can stay up past ~10k blocks on
realistic Free-Tier hardware.

The `prune_blocks_below` primitive already exists in `Storage` (commit
`92b20db`), but it's **dormant** because the in-memory `Vec<Block>`
would diverge from disk: pruning on disk doesn't pull blocks out of RAM.
This refactor closes the loop.

## Goals (P0)

1. **Bounded memory** — `Blockchain` instance stays under ~500 MB RSS
   regardless of chain length, even at 1M+ blocks.
2. **No consensus semantics change** — state roots, block hashes, fork
   choice, finality remain bit-identical to today's chain.
3. **Read API preserved** — call sites get a `Cow<Block>` or `Block`
   (owned), they don't have to know whether it came from cache or disk.
4. **Activate runtime pruning** — `--prune-keep-blocks N` (default 10000)
   actually deletes blocks below `latest - N` from both RAM and redb.

## Non-goals

- Schema change in redb (`blocks` table already keyed by height).
- Distributed block-store (sharded across nodes). Out of scope; this is
  a single-node memory fix.
- Async block fetch from peers for archival queries. If a pruned block
  is requested, return 410 Gone; let the caller find an archive node.

## Sites to migrate

Found via `grep -n "self\.blocks" src/core/chain.rs`:

| Line | Pattern | Phase |
|------|---------|-------|
| 313  | `pub blocks: Vec<Block>` | A (struct change) |
| 386  | `blocks: Vec<Block>` (internal field of replicated struct) | A |
| 913  | `self.blocks.len() as u64 - 1` (current height) | B (read) |
| 927  | `&self.blocks[0].hash` (genesis hash) | B (read) |
| 1149 | `self.blocks[..=h].to_vec()` (snapshot serialization) | B (read range) |
| 1351 | `self.blocks.get(manifest.height as usize)` | B (read optional) |
| 1368 | `self.blocks.get(h)` | B (read optional) |
| 1394 | `self.blocks.iter().skip(1)` | B (iter) |
| 1492 | `self.blocks.iter().rev()` | B (iter rev) |
| 2049 | `self.blocks.push(block.clone())` | C (write) |
| 2178 | `self.blocks.iter().skip(1)` | B (iter) |
| 2272 | `self.blocks[0].hash` (fallback) | B (read) |
| 3133 | `self.blocks[0].hash` (compare to genesis) | B (read) |
| 4320 | `let blocks = self.blocks.clone()` (full clone) | B (clone alternative) |

The two genesis reads (lines 927, 2272, 3133) need an `always-in-cache`
guarantee — never evict block 0.

The `self.blocks.iter().rev()` at line 1492 is `find_block_for_state_root`
which scans backward looking for a matching state root. With paginated
storage this becomes O(n disk reads) — needs to be replaced with a
direct lookup via a `state_root → height` redb index. Add that as a
sub-task.

## Design — `BlockStoreCursor`

```rust
// src/core/block_store.rs (new module)

use crate::core::Block;
use crate::storage::{Storage, StorageError};
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Arc;

/// Bounded view into the canonical chain. Always keeps genesis pinned;
/// keeps the most recently accessed `cache_size` other blocks in RAM.
/// Older blocks are read from redb on demand.
pub struct BlockStoreCursor {
    storage: Arc<Storage>,
    /// Genesis block, always resident.
    genesis: Block,
    /// LRU cache of non-genesis blocks. Bounded by cache_size.
    cache: LruCache<u64, Block>,
    /// Total chain length (height of head + 1). Tracked separately so
    /// `len()` doesn't require a disk roundtrip.
    height_count: u64,
    /// Lowest height still present in redb. Blocks below this have been pruned.
    base_height: u64,
}

impl BlockStoreCursor {
    pub fn new(storage: Arc<Storage>, cache_size: usize) -> Result<Self, StorageError> {
        let genesis = storage
            .get_block(0)?
            .ok_or(StorageError::MissingGenesis)?;
        let height_count = storage.latest_block_height()? + 1;
        let base_height = storage.lowest_block_height()?.unwrap_or(0);
        Ok(Self {
            storage,
            genesis,
            cache: LruCache::new(NonZeroUsize::new(cache_size).unwrap()),
            height_count,
            base_height,
        })
    }

    pub fn genesis(&self) -> &Block { &self.genesis }
    pub fn len(&self) -> u64 { self.height_count }
    pub fn is_empty(&self) -> bool { self.height_count == 0 }
    pub fn base_height(&self) -> u64 { self.base_height }
    pub fn head_height(&self) -> u64 { self.height_count.saturating_sub(1) }

    /// Returns block at `height`, reading from cache or redb.
    /// Returns None if height is below base_height (pruned) or above head.
    pub fn block_at(&mut self, height: u64) -> Result<Option<Block>, StorageError> {
        if height == 0 {
            return Ok(Some(self.genesis.clone()));
        }
        if height < self.base_height || height >= self.height_count {
            return Ok(None);
        }
        if let Some(b) = self.cache.get(&height) {
            return Ok(Some(b.clone()));
        }
        match self.storage.get_block(height)? {
            Some(b) => {
                self.cache.put(height, b.clone());
                Ok(Some(b))
            }
            None => Ok(None),
        }
    }

    /// Append a new block. Persists, caches, increments counter.
    pub fn append(&mut self, block: Block) -> Result<(), StorageError> {
        let h = block.header.height;
        debug_assert_eq!(h, self.height_count, "non-contiguous append");
        self.storage.put_block(&block)?;
        self.cache.put(h, block);
        self.height_count += 1;
        Ok(())
    }

    /// Iterate over `from..=to` (inclusive on both ends). Each iteration
    /// step touches cache then redb. Cheap for hot ranges; not designed
    /// for full-chain replay (use stream API for that).
    pub fn range(&mut self, from: u64, to: u64) -> impl Iterator<Item = Block> + '_ {
        (from..=to.min(self.head_height())).filter_map(move |h| {
            self.block_at(h).ok().flatten()
        })
    }

    /// Snapshot the full chain (used for state sync). Allocates O(len)
    /// memory — only safe to call for snapshot serialization, not as a
    /// hot path operation.
    pub fn collect_to(&mut self, head: u64) -> Result<Vec<Block>, StorageError> {
        let mut out = Vec::with_capacity((head + 1) as usize);
        for h in 0..=head {
            if let Some(b) = self.block_at(h)? {
                out.push(b);
            } else {
                return Err(StorageError::MissingBlock(h));
            }
        }
        Ok(out)
    }

    /// Drop all blocks below `keep_from` from both cache and redb.
    /// Returns the number of blocks pruned.
    pub fn prune_below(&mut self, keep_from: u64) -> Result<usize, StorageError> {
        if keep_from <= self.base_height + 1 {
            return Ok(0);
        }
        let removed = self.storage.prune_blocks_below(keep_from)?;
        self.base_height = keep_from;
        // Evict pruned heights from cache.
        let to_evict: Vec<u64> = self.cache.iter()
            .filter(|(h, _)| **h < keep_from)
            .map(|(h, _)| *h)
            .collect();
        for h in to_evict { self.cache.pop(&h); }
        Ok(removed)
    }
}
```

Dependency to add: `lru = "0.12"` (or current).

## Phases

### Phase A — Scaffolding (1 day)

1. Create `src/core/block_store.rs` with the `BlockStoreCursor` above.
2. Add `lru` to `[dependencies]` in `Cargo.toml`.
3. Add a parallel field `cursor: Option<BlockStoreCursor>` to `Blockchain`,
   constructed alongside `blocks: Vec<Block>` when storage is available.
4. Unit-test `BlockStoreCursor` in isolation: append, block_at, prune, LRU
   eviction.
5. **Do not touch any call site yet.** `blocks` and `cursor` coexist
   redundantly. CI should stay green.

### Phase B — Migrate read sites (2–3 days)

For each of the 12 read sites in the table above:

1. Change the call site from `self.blocks[...]` / `self.blocks.iter()` /
   `self.blocks.get(...)` to the cursor equivalent.
2. **Run the full test suite after each migration.** A bug here would
   silently diverge state roots.
3. Touch unrelated tests minimally — most should pass unchanged because
   cursor's `block_at` returns the same `Block` value.

Special cases:
- Line 1492 (`iter().rev()` for state-root lookup): replace with a new
  `state_root_to_height` index in redb. Schema bump (v4 → v5 within redb).
- Line 1149 (full snapshot): use `cursor.collect_to(snapshot_height)`.
  Document that this allocates `O(snapshot_height)` and is rate-limited
  upstream.

### Phase C — Migrate the write site (1 day)

Line 2049 (`self.blocks.push`): replace with `cursor.append(block.clone())`.
This already persists synchronously. The existing async persistence layer
becomes redundant for blocks (it stays for state, contracts, receipts).

After this phase, the `blocks: Vec<Block>` field becomes dead — delete it.

### Phase D — Activate pruning (1 day)

1. Wire `--prune-keep-blocks N` (CLI flag already exists, currently a
   no-op) to call `cursor.prune_below(head - N)` every N blocks.
2. `--archival` (default true) skips pruning entirely.
3. Add a runtime metric `curs3d_chain_base_height` so Grafana can show
   how aggressively the node is pruning.

### Phase E — Validation (1–2 days localnet soak)

1. Bring up a 2-node localnet with `--prune-keep-blocks 100`.
2. Run a tx-flood for 1 hour; expect ~360 blocks (10 s production).
3. Verify with `heaptrack` (or `tikv-jemallocator` + `MALLOC_STATS`)
   that RSS stays < 500 MB on the pruning node, while an archival
   companion node retains everything.
4. Restart the pruning node from cold (`systemctl restart curs3d`):
   chain must rebuild state from the most recent post-prune block,
   without crashing on "missing block 1".
5. Trigger a snapshot sync between the two nodes after restart: the
   archival node must serve the snapshot correctly even though the
   pruned node has no blocks below the snapshot height.

## Risk register

| Risk | Severity | Mitigation |
|------|----------|------------|
| State-root divergence after a partial migration | Catastrophic | Run full test suite between each read-site migration; never commit a half-migrated chain.rs. |
| Cache thrashing under cold-start (every state-root recompute reads from disk) | High | LRU size = 1000 blocks by default. Tune via `--block-cache-size`. |
| `iter().rev()` scan at line 1492 becomes O(chain_length) disk reads | High | Build a `state_root → height` index in redb (Phase B sub-task). |
| Restart loses in-memory state (mempool, pending votes) | Already true today | No change; this refactor is about chain history, not consensus state. |
| `cursor.collect_to(huge_height)` OOM during snapshot | Medium | Add a hard cap; deny snapshots > 1M blocks at the network layer. |
| LRU eviction races with `replace_blocks` (reorg) | Medium | Reorgs invalidate cache entries above the fork point; add `cursor.invalidate_from(h)`. |

## Acceptance criteria

This refactor is "done" when:

1. `grep -n "blocks: Vec<Block>" src/core/chain.rs` returns nothing.
2. `cargo test --lib` passes (full suite, 213+).
3. A localnet validator with `--prune-keep-blocks 100` survives 24 h
   under tx-flood with RSS < 500 MB.
4. The archival counterpart serves a snapshot to the pruning node after
   the latter cold-starts.
5. `prune_blocks_below` is callable at runtime, not just from a unit test.

## Out-of-scope future work

- Distributed block-store (multi-node sharding) — irrelevant for L1.
- Snapshot deltas (sync only the blocks since last snapshot) — separate
  optimization, not required for correctness.
- Compression of blocks at rest in redb — measure first; gzip might cost
  more CPU than it saves disk.
