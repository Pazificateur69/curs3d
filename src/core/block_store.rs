//! Paginated block store — bounded RAM view onto the canonical chain.
//!
//! Replaces the existing `Blockchain::blocks: Vec<Block>` (see refactor
//! plan in `docs/REFACTOR_BLOCK_STORE.md`). All methods take `&self` and
//! lock an interior `Mutex` so the cursor can be embedded behind
//! `Arc<Mutex<Blockchain>>` (the shape used by the network layer) without
//! forcing every chain helper to be `&mut self`.
//!
//! Invariants:
//! - Genesis (height 0) is always resident — never evicted from cache.
//! - `state.height_count` is the authoritative "how many blocks have ever
//!   been appended" counter; it does NOT decrement when pruning.
//! - `state.base_height` is the lowest height still present in redb.
//!   Blocks below this have been pruned and `block_at` returns `Ok(None)`.

use crate::core::block::Block;
use crate::storage::{BlockBackend, StorageError};
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

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
    #[error("cursor mutex poisoned")]
    Poisoned,
}

/// Interior state guarded by the cursor's single Mutex.
struct CursorState {
    /// Genesis block, pinned (never evicted, never reloaded from disk).
    genesis: Block,
    /// LRU cache of non-genesis blocks.
    cache: LruCache<u64, Block>,
    /// `(head_height + 1)` — total blocks ever appended.
    height_count: u64,
    /// Lowest height still readable from redb. Reads below this fail with
    /// `Ok(None)`. Bumped by `prune_below`.
    base_height: u64,
}

pub struct BlockStoreCursor {
    storage: Arc<dyn BlockBackend>,
    state: Mutex<CursorState>,
    /// When true, [`Self::append`] persists the block to `storage` before
    /// updating the in-memory cache. Use this when the cursor's backend is
    /// the only durable home for blocks (e.g. an `InMemoryBlockBackend`
    /// behind a storage-less chain). Leave false when a separate
    /// persistence pipeline (e.g. `chain::persist_added_block`) already
    /// writes to storage — double-writing through both paths would also
    /// break the async-persistence invariant
    /// (`test_async_persistence_does_not_write_on_each_block`).
    self_persist: bool,
}

/// Short-hand for the lock pattern.
macro_rules! lock_state {
    ($self:expr) => {
        $self.state.lock().map_err(|_| BlockStoreError::Poisoned)?
    };
}

impl BlockStoreCursor {
    /// Construct a cursor over any `BlockBackend`. Loads genesis eagerly.
    /// Returns `MissingGenesis` if storage has not been initialized with
    /// at least one block (height 0).
    ///
    /// Append-time persistence is **disabled** — the caller is expected
    /// to write blocks to storage through a separate pipeline (this is
    /// the redb path in `Blockchain::with_storage_mode`).
    pub fn new(storage: Arc<dyn BlockBackend>, cache_size: usize) -> Result<Self, BlockStoreError> {
        Self::new_with_persist(storage, cache_size, false)
    }

    /// Same as [`Self::new`] but enables append-time persistence: every
    /// [`Self::append`] call writes the block to `storage` before
    /// inserting it into the cache. Use for storage-less chains where the
    /// cursor's backend (typically `InMemoryBlockBackend`) is the only
    /// place blocks live durably.
    pub fn new_self_persisting(
        storage: Arc<dyn BlockBackend>,
        cache_size: usize,
    ) -> Result<Self, BlockStoreError> {
        Self::new_with_persist(storage, cache_size, true)
    }

    fn new_with_persist(
        storage: Arc<dyn BlockBackend>,
        cache_size: usize,
        self_persist: bool,
    ) -> Result<Self, BlockStoreError> {
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
            state: Mutex::new(CursorState {
                genesis,
                cache,
                height_count,
                base_height,
            }),
            self_persist,
        })
    }

    pub fn genesis(&self) -> Result<Block, BlockStoreError> {
        Ok(lock_state!(self).genesis.clone())
    }

    /// Total number of blocks ever appended (= head_height + 1).
    pub fn len(&self) -> Result<u64, BlockStoreError> {
        Ok(lock_state!(self).height_count)
    }

    pub fn is_empty(&self) -> Result<bool, BlockStoreError> {
        Ok(lock_state!(self).height_count == 0)
    }

    /// Head height. Returns 0 for a chain with only genesis.
    pub fn head_height(&self) -> Result<u64, BlockStoreError> {
        Ok(lock_state!(self).height_count.saturating_sub(1))
    }

    /// Lowest height present in storage. Reads below this return Ok(None).
    pub fn base_height(&self) -> Result<u64, BlockStoreError> {
        Ok(lock_state!(self).base_height)
    }

    /// Fetch the block at `height`. Cache hit returns a clone of the
    /// cached value; miss reads from redb and inserts into cache.
    ///
    /// Returns `Ok(None)` if the height has been pruned (below
    /// base_height) or is above the head.
    pub fn block_at(&self, height: u64) -> Result<Option<Block>, BlockStoreError> {
        // Snapshot the bounds + cache hit under the lock, then release
        // the lock before the (potentially blocking) storage I/O.
        let cached_or_bounds = {
            let mut state = lock_state!(self);
            if height == 0 {
                return Ok(Some(state.genesis.clone()));
            }
            if height >= state.height_count || height < state.base_height {
                return Ok(None);
            }
            state.cache.get(&height).cloned()
        };
        if let Some(b) = cached_or_bounds {
            return Ok(Some(b));
        }

        let from_storage = self.storage.get_block(height)?;
        if let Some(b) = from_storage.clone() {
            // Re-acquire lock to insert. Race tolerated: if two readers
            // miss concurrently, both call storage and one wins the put.
            let mut state = lock_state!(self);
            // Validate bounds again under the second lock — a concurrent
            // prune may have shifted them.
            if height >= state.height_count || height < state.base_height {
                return Ok(None);
            }
            state.cache.put(height, b);
        }
        Ok(from_storage)
    }

    /// Append a new block to the cursor's view. The block's `header.height`
    /// MUST equal `self.len()` (i.e. one above the current head).
    ///
    /// Behavior depends on the construction mode:
    /// - default ([`Self::new`]): only updates the in-memory cache +
    ///   counter. Storage persistence is the caller's responsibility
    ///   (chain.rs handles it via `persist_added_block`). Avoids
    ///   double-writing in the redb path and preserves the
    ///   async-persistence invariant tested by
    ///   `test_async_persistence_does_not_write_on_each_block`.
    /// - self-persisting ([`Self::new_self_persisting`]): persists to
    ///   `storage` first, then updates the cache. Required when the
    ///   cursor's backend is the only durable storage (storage-less
    ///   chains) — without it, an LRU eviction could lose history.
    pub fn append(&self, block: Block) -> Result<(), BlockStoreError> {
        let h = block.header.height;
        let mut state = lock_state!(self);
        if h != state.height_count {
            return Err(BlockStoreError::NonContiguousAppend {
                expected: state.height_count,
                actual: h,
            });
        }
        if self.self_persist {
            self.storage.put_block(&block)?;
        }
        if h == 0 {
            state.genesis = block;
        } else {
            state.cache.put(h, block);
        }
        state.height_count += 1;
        Ok(())
    }

    /// Drop all blocks below `keep_from` from both redb and the cache.
    /// Genesis is never dropped. Returns the number of blocks removed.
    pub fn prune_below(&self, keep_from: u64) -> Result<usize, BlockStoreError> {
        if keep_from == 0 {
            return Ok(0);
        }
        let need_prune = {
            let state = lock_state!(self);
            keep_from > state.base_height
        };
        if !need_prune {
            return Ok(0);
        }
        let removed = self.storage.prune_blocks_below(keep_from)?;
        let mut state = lock_state!(self);
        state.base_height = keep_from;
        let to_evict: Vec<u64> = state
            .cache
            .iter()
            .filter(|(h, _)| **h < keep_from)
            .map(|(h, _)| *h)
            .collect();
        for h in to_evict {
            state.cache.pop(&h);
        }
        Ok(removed)
    }

    /// Invalidate cache entries at or above `from` and truncate the
    /// height counter. Used by reorg paths.
    pub fn invalidate_from(&self, from: u64) -> Result<(), BlockStoreError> {
        let mut state = lock_state!(self);
        let to_evict: Vec<u64> = state
            .cache
            .iter()
            .filter(|(h, _)| **h >= from)
            .map(|(h, _)| *h)
            .collect();
        for h in to_evict {
            state.cache.pop(&h);
        }
        if from < state.height_count {
            state.height_count = from;
        }
        Ok(())
    }

    /// Number of currently cached blocks (excluding pinned genesis).
    pub fn cache_size(&self) -> Result<usize, BlockStoreError> {
        Ok(lock_state!(self).cache.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::block::Block;
    use crate::storage::InMemoryBlockBackend;

    /// Test fixture: an `InMemoryBlockBackend` boxed as `Arc<dyn BlockBackend>`.
    /// Bypasses redb tmpdir + fsync — tests run much faster than with the
    /// concrete `Storage`.
    fn open_test_storage() -> (Arc<dyn BlockBackend>, ()) {
        let storage: Arc<dyn BlockBackend> = Arc::new(InMemoryBlockBackend::new());
        (storage, ())
    }

    fn seed_genesis(storage: &dyn BlockBackend) -> Block {
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
        let g = seed_genesis(storage.as_ref());

        let cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
        assert_eq!(cursor.len().unwrap(), 1);
        assert_eq!(cursor.head_height().unwrap(), 0);
        assert_eq!(cursor.base_height().unwrap(), 0);
        assert_eq!(cursor.genesis().unwrap().hash, g.hash);
    }

    #[test]
    fn cursor_append_persists_and_caches() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(storage.as_ref());

        let cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
        let b1 = synthetic_block(&g);
        cursor.append(b1.clone()).expect("append");
        assert_eq!(cursor.len().unwrap(), 2);
        assert_eq!(cursor.head_height().unwrap(), 1);
        let read = cursor.block_at(1).expect("read").expect("present");
        assert_eq!(read.hash, b1.hash);
        assert_eq!(cursor.cache_size().unwrap(), 1);
    }

    #[test]
    fn cursor_rejects_non_contiguous_append() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
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
        seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
        let g = cursor.block_at(0).expect("ok").expect("genesis present");
        assert_eq!(g.header.height, 0);
    }

    #[test]
    fn cursor_block_at_above_head_returns_none() {
        let (storage, _dir) = open_test_storage();
        seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(storage, 8).expect("cursor");
        assert!(cursor.block_at(1).expect("ok").is_none());
        assert!(cursor.block_at(99).expect("ok").is_none());
    }

    #[test]
    fn cursor_lru_evicts_oldest_when_full() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(Arc::clone(&storage), 2).expect("cursor");

        let mut parent = g;
        for _ in 0..5 {
            let b = synthetic_block(&parent);
            // `cursor.append` only updates the in-memory cache; persistence is
            // the chain layer's job. For these standalone cursor tests we
            // also `put_block` so an evicted height can be reloaded from
            // storage on the next `block_at`.
            storage.put_block(&b).expect("persist");
            cursor.append(b.clone()).expect("append");
            parent = b;
        }
        assert!(cursor.cache_size().unwrap() <= 2);
        let b1 = cursor.block_at(1).expect("ok").expect("present");
        assert_eq!(b1.header.height, 1);
    }

    #[test]
    fn cursor_prune_below_drops_blocks() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(storage, 64).expect("cursor");

        let mut parent = g;
        for _ in 0..10 {
            let b = synthetic_block(&parent);
            cursor.append(b.clone()).expect("append");
            parent = b;
        }
        let removed = cursor.prune_below(5).expect("prune");
        assert!(removed > 0, "should have pruned heights 1..5");
        assert_eq!(cursor.base_height().unwrap(), 5);

        assert!(cursor.block_at(2).expect("ok").is_none());
        assert!(cursor.block_at(5).expect("ok").is_some());
        assert!(cursor.block_at(9).expect("ok").is_some());
        assert!(cursor.block_at(0).expect("ok").is_some());
    }

    #[test]
    fn cursor_invalidate_from_truncates_height_and_cache() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(storage, 16).expect("cursor");

        let mut parent = g;
        for _ in 0..5 {
            let b = synthetic_block(&parent);
            cursor.append(b.clone()).expect("append");
            parent = b;
        }
        assert_eq!(cursor.head_height().unwrap(), 5);

        cursor.invalidate_from(3).expect("invalidate");
        assert_eq!(cursor.head_height().unwrap(), 2);
        assert_eq!(cursor.len().unwrap(), 3);
        for h in 3..=5 {
            assert!(cursor.block_at(h).expect("ok").is_none());
        }
    }

    // ─── Property-based tests (task #33) ─────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn prop_append_then_read_roundtrip(
            cache_size in 1usize..32,
            count in 1u8..40,
        ) {
            let (storage, _dir) = open_test_storage();
            let g = seed_genesis(storage.as_ref());
            let cursor = BlockStoreCursor::new(Arc::clone(&storage), cache_size).expect("cursor");

            let mut parent = g.clone();
            let mut appended_hashes = vec![g.hash.clone()];
            for _ in 0..count {
                let b = synthetic_block(&parent);
                appended_hashes.push(b.hash.clone());
                // Persist + cursor update (mirrors chain's dual-write pattern).
                storage.put_block(&b).expect("persist");
                cursor.append(b.clone()).expect("append");
                parent = b;
            }

            for h in 0..=count as u64 {
                let read = cursor.block_at(h).expect("ok").expect("present");
                prop_assert_eq!(&read.hash, &appended_hashes[h as usize]);
            }
        }

        #[test]
        fn prop_prune_preserves_above_threshold(
            count in 5u8..30,
            keep_from in 1u64..15,
        ) {
            let (storage, _dir) = open_test_storage();
            let g = seed_genesis(storage.as_ref());
            let cursor = BlockStoreCursor::new(Arc::clone(&storage), 64).expect("cursor");

            let mut parent = g;
            let mut hashes = vec![parent.hash.clone()];
            for _ in 0..count {
                let b = synthetic_block(&parent);
                hashes.push(b.hash.clone());
                storage.put_block(&b).expect("persist");
                cursor.append(b.clone()).expect("append");
                parent = b;
            }

            let keep = keep_from.min(count as u64);
            cursor.prune_below(keep).expect("prune");
            for h in keep..=count as u64 {
                let read = cursor.block_at(h).expect("ok").expect("present after prune");
                prop_assert_eq!(&read.hash, &hashes[h as usize]);
            }
            prop_assert!(cursor.block_at(0).expect("ok").is_some());
        }

        #[test]
        fn prop_non_contiguous_append_errors(skip in 2u64..10) {
            let (storage, _dir) = open_test_storage();
            let g = seed_genesis(storage.as_ref());
            let cursor = BlockStoreCursor::new(storage, 8).expect("cursor");

            let mut bad = synthetic_block(&g);
            bad.header.height = skip;
            let result = cursor.append(bad);
            let is_expected = matches!(result, Err(BlockStoreError::NonContiguousAppend { .. }));
            prop_assert!(is_expected);
        }
    }

    // ─── Concurrent + edge-case tests ────────────────────────────────

    /// Cursor is `Send + Sync` (Mutex inside). Concurrent block_at from
    /// many threads must not race the cache or panic the mutex.
    #[test]
    fn cursor_concurrent_block_at_is_safe() {
        use std::thread;

        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(storage.as_ref());
        let cursor = Arc::new(BlockStoreCursor::new(Arc::clone(&storage), 4).expect("cursor"));

        // Seed 20 blocks.
        let mut parent = g;
        for _ in 0..20 {
            let b = synthetic_block(&parent);
            storage.put_block(&b).expect("persist");
            cursor.append(b.clone()).expect("append");
            parent = b;
        }

        // Spawn 8 threads, each reads every height. None must panic.
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let cursor = Arc::clone(&cursor);
                thread::spawn(move || {
                    for h in 0..=20u64 {
                        let b = cursor.block_at(h).expect("ok").expect("present");
                        assert_eq!(b.header.height, h);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("thread did not panic");
        }
    }

    /// After invalidate_from(0) the cursor reports len=0; re-appending
    /// from scratch must work and read back consistently. Mirrors the
    /// chain's reorg/snapshot path that replace_all_blocks triggers.
    #[test]
    fn cursor_reorg_via_invalidate_then_reappend() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(Arc::clone(&storage), 8).expect("cursor");

        let mut parent = g.clone();
        for _ in 0..5 {
            let b = synthetic_block(&parent);
            storage.put_block(&b).expect("persist");
            cursor.append(b.clone()).expect("append");
            parent = b;
        }
        assert_eq!(cursor.len().unwrap(), 6);

        // Reorg: throw away everything from height 0 onwards.
        cursor.invalidate_from(0).expect("invalidate");
        assert_eq!(cursor.len().unwrap(), 0);

        // Re-append from genesis. The cursor's `append` requires the
        // first block's height to match its `height_count` (now 0).
        let mut new_g = Block::genesis();
        new_g.hash = vec![0xaau8; 32];
        cursor.append(new_g.clone()).expect("re-append genesis");
        let mut new_parent = new_g;
        for _ in 0..3 {
            let b = synthetic_block(&new_parent);
            storage.put_block(&b).expect("persist");
            cursor.append(b.clone()).expect("append");
            new_parent = b;
        }
        assert_eq!(cursor.len().unwrap(), 4);
        assert_eq!(cursor.head_height().unwrap(), 3);

        // Genesis through cursor must match the re-appended block (the
        // cursor accepted height-0 as the new genesis under invalidate semantics).
        let g_read = cursor.block_at(0).expect("ok").expect("present");
        assert_eq!(g_read.hash, vec![0xaau8; 32]);
    }

    /// Pruning while a concurrent reader is walking the chain must not
    /// produce stale Block instances for pruned heights.
    #[test]
    fn cursor_prune_then_read_pruned_returns_none() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(Arc::clone(&storage), 64).expect("cursor");

        let mut parent = g;
        for _ in 0..20 {
            let b = synthetic_block(&parent);
            storage.put_block(&b).expect("persist");
            cursor.append(b.clone()).expect("append");
            parent = b;
        }

        cursor.prune_below(10).expect("prune");
        for h in 1..10 {
            assert!(
                cursor.block_at(h).expect("ok").is_none(),
                "pruned height {h} must report None",
            );
        }
        for h in 10..=20 {
            assert!(
                cursor.block_at(h).expect("ok").is_some(),
                "post-prune height {h} must still be present",
            );
        }
        // Genesis is always present, even after a deep prune.
        assert!(cursor.block_at(0).expect("ok").is_some());
    }

    /// In self-persisting mode, the cursor writes to the backend on every
    /// append. Blocks evicted from the LRU cache must therefore still be
    /// retrievable via the backend. This is the invariant that lets
    /// storage-less chains drop `self.blocks` (Phase D.3 of #28) without
    /// losing history when the cache turns over.
    #[test]
    fn cursor_self_persist_survives_lru_eviction() {
        let (storage, _dir) = open_test_storage();
        seed_genesis(storage.as_ref());
        let cursor =
            BlockStoreCursor::new_self_persisting(Arc::clone(&storage), 2).expect("cursor");

        // Append 10 blocks with a cache that holds only 2. Without
        // self-persist, heights 1..=7 would all be lost (cache evicted,
        // backend never written).
        let mut parent = cursor.genesis().unwrap();
        let mut hashes = vec![parent.hash.clone()];
        for _ in 0..10 {
            let b = synthetic_block(&parent);
            hashes.push(b.hash.clone());
            cursor.append(b.clone()).expect("append");
            parent = b;
        }
        assert_eq!(cursor.len().unwrap(), 11);
        assert!(cursor.cache_size().unwrap() <= 2);

        // Every height (including evicted ones) must still resolve.
        for h in 0..=10 {
            let read = cursor
                .block_at(h)
                .expect("ok")
                .unwrap_or_else(|| panic!("evicted height {h} should still be retrievable"));
            assert_eq!(read.hash, hashes[h as usize]);
        }
    }

    /// Default (non-self-persisting) cursor must NOT write to the backend.
    /// Verifying this protects the async-persistence invariant in the
    /// redb-backed chain path.
    #[test]
    fn cursor_default_mode_does_not_persist_on_append() {
        let (storage, _dir) = open_test_storage();
        let g = seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(Arc::clone(&storage), 8).expect("cursor");

        let b1 = synthetic_block(&g);
        cursor.append(b1.clone()).expect("append");

        // Cursor's cache sees b1, but backend was never written.
        assert!(cursor.block_at(1).expect("ok").is_some());
        assert!(
            storage.get_block(1).expect("ok").is_none(),
            "default-mode cursor must not write to backend on append"
        );
    }

    /// `is_empty` matches `len() == 0` across the cursor lifecycle.
    #[test]
    fn cursor_is_empty_consistent_with_len() {
        let (storage, _dir) = open_test_storage();
        seed_genesis(storage.as_ref());
        let cursor = BlockStoreCursor::new(Arc::clone(&storage), 4).expect("cursor");

        assert!(!cursor.is_empty().unwrap());
        cursor.invalidate_from(0).expect("invalidate");
        assert!(cursor.is_empty().unwrap());
        assert_eq!(cursor.len().unwrap(), 0);
    }
}
