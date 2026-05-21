//! Paginated block store — bounded RAM view onto the canonical chain.
//!
//! Replaces the existing `Blockchain::blocks: Vec<Block>` (see refactor
//! plan in `docs/REFACTOR_BLOCK_STORE.md`). This module is **scaffolding
//! only** at this revision: it compiles, has unit tests, and is wired
//! into `core::mod`, but is NOT yet plugged into `Blockchain`. That
//! migration is phases B–D.
//!
//! Invariants:
//! - Genesis (height 0) is always resident — never evicted from cache.
//! - `height_count` is the authoritative "how many blocks have ever been
//!   appended" counter; it does NOT decrement when pruning.
//! - `base_height` is the lowest height still present in redb. Blocks
//!   below this have been pruned and `block_at` returns `Ok(None)`.

use crate::core::block::Block;
use crate::storage::{Storage, StorageError};
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Arc;

/// Default LRU cache size — number of non-genesis blocks kept in RAM.
/// 1000 × ~3 KB average = ~3 MB RAM per validator. Tunable via
/// `BlockStoreCursor::new`.
pub const DEFAULT_BLOCK_CACHE_SIZE: usize = 1000;

#[derive(Debug, thiserror::Error)]
pub enum BlockStoreError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("missing genesis block in storage")]
    MissingGenesis,
    #[error("missing block at height {0} (expected present)")]
    MissingBlock(u64),
    #[error("non-contiguous append: expected height {expected}, got {actual}")]
    NonContiguousAppend { expected: u64, actual: u64 },
}

pub struct BlockStoreCursor {
    storage: Arc<Storage>,
    /// Genesis block, pinned in memory.
    genesis: Block,
    /// LRU cache of non-genesis blocks.
    cache: LruCache<u64, Block>,
    /// `(head_height + 1)` — total blocks ever appended.
    height_count: u64,
    /// Lowest height still readable from redb. Reads below this fail with
    /// `Ok(None)`. Bumped by `prune_below`.
    base_height: u64,
}

impl BlockStoreCursor {
    /// Construct a cursor over an open `Storage`. Loads genesis eagerly.
    /// Returns `MissingGenesis` if storage has not been initialized with
    /// at least one block (height 0).
    pub fn new(storage: Arc<Storage>, cache_size: usize) -> Result<Self, BlockStoreError> {
        let genesis = storage
            .get_block(0)?
            .ok_or(BlockStoreError::MissingGenesis)?;

        let head_height_opt = storage.get_height()?;
        let height_count = head_height_opt.map(|h| h + 1).unwrap_or(1);

        // Walk forward from 1 to find the first block that exists, to
        // discover the prune watermark. Cheap on the happy path (block 1
        // is present) — expensive only after pruning, which is rare.
        let mut base_height = 0;
        if height_count > 1 {
            for candidate in 1..height_count {
                if storage.get_block(candidate)?.is_some() {
                    base_height = candidate;
                    break;
                }
            }
            // Edge case: all non-genesis blocks pruned. base_height stays 0
            // (genesis is always there) but the cursor will report
            // block_at(h>0) as None.
        }

        let cache = LruCache::new(
            NonZeroUsize::new(cache_size.max(1)).expect("cache_size >= 1 enforced by max(1)"),
        );

        Ok(Self {
            storage,
            genesis,
            cache,
            height_count,
            base_height,
        })
    }

    pub fn genesis(&self) -> &Block {
        &self.genesis
    }

    /// Total number of blocks ever appended (= head_height + 1).
    pub fn len(&self) -> u64 {
        self.height_count
    }

    pub fn is_empty(&self) -> bool {
        self.height_count == 0
    }

    /// Head height. Returns 0 for a chain with only genesis.
    pub fn head_height(&self) -> u64 {
        self.height_count.saturating_sub(1)
    }

    /// Lowest height present in storage. Reads below this return Ok(None).
    pub fn base_height(&self) -> u64 {
        self.base_height
    }

    /// Fetch the block at `height`. Cache hit returns a clone of the
    /// cached value; miss reads from redb and inserts into cache.
    ///
    /// Returns `Ok(None)` if the height has been pruned (below
    /// base_height) or is above the head.
    pub fn block_at(&mut self, height: u64) -> Result<Option<Block>, BlockStoreError> {
        if height == 0 {
            return Ok(Some(self.genesis.clone()));
        }
        if height >= self.height_count {
            return Ok(None);
        }
        if height < self.base_height {
            return Ok(None);
        }
        if let Some(cached) = self.cache.get(&height) {
            return Ok(Some(cached.clone()));
        }
        match self.storage.get_block(height)? {
            Some(b) => {
                self.cache.put(height, b.clone());
                Ok(Some(b))
            }
            None => Ok(None),
        }
    }

    /// Append a new block. The block's `header.height` MUST equal
    /// `self.height_count` (i.e. one above the current head). Persists
    /// to redb and inserts into cache.
    pub fn append(&mut self, block: Block) -> Result<(), BlockStoreError> {
        let h = block.header.height;
        if h != self.height_count {
            return Err(BlockStoreError::NonContiguousAppend {
                expected: self.height_count,
                actual: h,
            });
        }
        self.storage.put_block(&block)?;
        if h == 0 {
            // Replacing genesis through append is not how genesis is
            // supposed to be installed, but if it happens, keep state
            // consistent.
            self.genesis = block;
        } else {
            self.cache.put(h, block);
        }
        self.height_count += 1;
        Ok(())
    }

    /// Drop all blocks below `keep_from` from both redb and the cache.
    /// Genesis is never dropped (base_height is clamped to >= 1 for
    /// the in-memory state, but redb may not even have a height 0 slot
    /// if you never seeded it; we don't touch height 0 here).
    ///
    /// Returns the number of blocks actually removed from redb.
    pub fn prune_below(&mut self, keep_from: u64) -> Result<usize, BlockStoreError> {
        if keep_from == 0 {
            // No-op: pruning to 0 keeps everything.
            return Ok(0);
        }
        if keep_from <= self.base_height {
            return Ok(0);
        }
        let removed = self.storage.prune_blocks_below(keep_from)?;
        self.base_height = keep_from;
        // Evict pruned heights from cache.
        let to_evict: Vec<u64> = self
            .cache
            .iter()
            .filter(|(h, _)| **h < keep_from)
            .map(|(h, _)| *h)
            .collect();
        for h in to_evict {
            self.cache.pop(&h);
        }
        Ok(removed)
    }

    /// Invalidate cache entries at or above `from`. Used during reorgs
    /// where `replace_blocks` is about to overwrite a suffix of the chain.
    pub fn invalidate_from(&mut self, from: u64) {
        let to_evict: Vec<u64> = self
            .cache
            .iter()
            .filter(|(h, _)| **h >= from)
            .map(|(h, _)| *h)
            .collect();
        for h in to_evict {
            self.cache.pop(&h);
        }
        if from < self.height_count {
            self.height_count = from;
        }
    }

    /// Number of currently cached blocks (excluding pinned genesis).
    pub fn cache_size(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::block::Block;
    use crate::storage::Storage;

    fn open_test_storage() -> (Arc<Storage>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage =
            Arc::new(Storage::open(dir.path().join("blockstore_test")).expect("open storage"));
        (storage, dir)
    }

    fn seed_genesis(storage: &Storage) -> Block {
        let genesis = Block::genesis();
        storage.put_block(&genesis).expect("put genesis");
        genesis
    }

    fn synthetic_block(parent: &Block) -> Block {
        let mut next = parent.clone();
        next.header.height = parent.header.height + 1;
        next.header.prev_hash = parent.hash.clone();
        next.hash = vec![next.header.height as u8; 32];
        next
    }

    #[test]
    fn cursor_loads_genesis_on_open() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(&storage);

        let cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
        assert_eq!(cursor.len(), 1);
        assert_eq!(cursor.head_height(), 0);
        assert_eq!(cursor.base_height(), 0);
        assert_eq!(cursor.genesis().hash, g.hash);
    }

    #[test]
    fn cursor_append_persists_and_caches() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(&storage);

        let mut cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
        let b1 = synthetic_block(&g);
        cursor.append(b1.clone()).expect("append");
        assert_eq!(cursor.len(), 2);
        assert_eq!(cursor.head_height(), 1);
        let read = cursor.block_at(1).expect("read").expect("present");
        assert_eq!(read.hash, b1.hash);
        assert_eq!(cursor.cache_size(), 1);
    }

    #[test]
    fn cursor_rejects_non_contiguous_append() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(&storage);
        let mut cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
        let b1 = synthetic_block(&g);
        let b2 = synthetic_block(&b1);
        let err = cursor.append(b2).expect_err("should reject");
        match err {
            BlockStoreError::NonContiguousAppend { expected, actual } => {
                assert_eq!(expected, 1);
                assert_eq!(actual, 2);
            }
            other => panic!("expected NonContiguousAppend, got {other:?}"),
        }
    }

    #[test]
    fn cursor_block_at_genesis_always_returns() {
        let (storage, _dir) = open_test_storage();
        seed_genesis(&storage);
        let mut cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
        let g = cursor.block_at(0).expect("ok").expect("genesis present");
        assert_eq!(g.header.height, 0);
    }

    #[test]
    fn cursor_block_at_above_head_returns_none() {
        let (storage, _dir) = open_test_storage();
        seed_genesis(&storage);
        let mut cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
        assert!(cursor.block_at(1).expect("ok").is_none());
        assert!(cursor.block_at(99).expect("ok").is_none());
    }

    #[test]
    fn cursor_lru_evicts_oldest_when_full() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(&storage);
        let mut cursor = BlockStoreCursor::new(storage, 2).expect("cursor");

        let mut parent = g;
        for _ in 0..5 {
            let b = synthetic_block(&parent);
            cursor.append(b.clone()).expect("append");
            parent = b;
        }
        // Cache cap = 2, appended 5 → only the 2 most recent survive.
        assert!(cursor.cache_size() <= 2);

        // Older blocks still readable (via redb refill).
        let b1 = cursor.block_at(1).expect("ok").expect("present");
        assert_eq!(b1.header.height, 1);
    }

    #[test]
    fn cursor_prune_below_drops_blocks() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(&storage);
        let mut cursor = BlockStoreCursor::new(storage, 64).expect("cursor");

        let mut parent = g;
        for _ in 0..10 {
            let b = synthetic_block(&parent);
            cursor.append(b.clone()).expect("append");
            parent = b;
        }
        // Prune everything below height 5.
        let removed = cursor.prune_below(5).expect("prune");
        assert!(removed > 0, "should have pruned heights 1..5");
        assert_eq!(cursor.base_height(), 5);

        // Below base_height = None.
        assert!(cursor.block_at(2).expect("ok").is_none());
        // At and above base_height = Some.
        assert!(cursor.block_at(5).expect("ok").is_some());
        assert!(cursor.block_at(9).expect("ok").is_some());
        // Genesis is still pinned even after prune.
        assert!(cursor.block_at(0).expect("ok").is_some());
    }

    #[test]
    fn cursor_invalidate_from_truncates_height_and_cache() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(&storage);
        let mut cursor = BlockStoreCursor::new(storage, 16).expect("cursor");

        let mut parent = g;
        for _ in 0..5 {
            let b = synthetic_block(&parent);
            cursor.append(b.clone()).expect("append");
            parent = b;
        }
        assert_eq!(cursor.head_height(), 5);

        cursor.invalidate_from(3);
        assert_eq!(cursor.head_height(), 2);
        assert_eq!(cursor.len(), 3);
        // Cache no longer reports heights >= 3.
        for h in 3..=5 {
            // Reading invalidated entries is `None` because height_count truncated.
            assert!(cursor.block_at(h).expect("ok").is_none());
        }
    }

    // ─── Property-based tests (task #33) ─────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// Every height appended is readable back, regardless of cache size.
        #[test]
        fn prop_append_then_read_roundtrip(
            cache_size in 1usize..32,
            count in 1u8..40,
        ) {
            let (storage, _dir) = open_test_storage();
            let g = seed_genesis(&storage);
            let mut cursor = BlockStoreCursor::new(storage, cache_size).expect("cursor");

            let mut parent = g.clone();
            let mut appended_hashes = vec![g.hash.clone()];
            for _ in 0..count {
                let b = synthetic_block(&parent);
                appended_hashes.push(b.hash.clone());
                cursor.append(b.clone()).expect("append");
                parent = b;
            }

            // Every height must read back the right hash.
            for h in 0..=count as u64 {
                let read = cursor.block_at(h).expect("ok").expect("present");
                prop_assert_eq!(&read.hash, &appended_hashes[h as usize]);
            }
        }

        /// Pruning never affects heights >= keep_from.
        #[test]
        fn prop_prune_preserves_above_threshold(
            count in 5u8..30,
            keep_from in 1u64..15,
        ) {
            let (storage, _dir) = open_test_storage();
            let g = seed_genesis(&storage);
            let mut cursor = BlockStoreCursor::new(storage, 64).expect("cursor");

            let mut parent = g;
            let mut hashes = vec![parent.hash.clone()];
            for _ in 0..count {
                let b = synthetic_block(&parent);
                hashes.push(b.hash.clone());
                cursor.append(b.clone()).expect("append");
                parent = b;
            }

            let keep = keep_from.min(count as u64);
            cursor.prune_below(keep).expect("prune");
            for h in keep..=count as u64 {
                let read = cursor.block_at(h).expect("ok").expect("present after prune");
                prop_assert_eq!(&read.hash, &hashes[h as usize]);
            }
            // Genesis still pinned.
            prop_assert!(cursor.block_at(0).expect("ok").is_some());
        }

        /// Non-contiguous append always fails.
        #[test]
        fn prop_non_contiguous_append_errors(skip in 2u64..10) {
            let (storage, _dir) = open_test_storage();
            let g = seed_genesis(&storage);
            let mut cursor = BlockStoreCursor::new(storage, 8).expect("cursor");

            let mut bad = synthetic_block(&g);
            bad.header.height = skip; // forces non-contiguous append
            let result = cursor.append(bad);
            let is_expected = matches!(result, Err(BlockStoreError::NonContiguousAppend { .. }));
            prop_assert!(is_expected);
        }
    }
}
