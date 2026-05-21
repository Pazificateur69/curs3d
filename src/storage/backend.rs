//! Storage trait — abstraction over the canonical block-level persistence
//! API. Lets `BlockStoreCursor` (and future consumers) accept any backend
//! that implements the same shape.
//!
//! This is the **minimal slice** of the full storage surface — only the
//! methods `BlockStoreCursor` actually needs. The full account/contract/
//! pending/governance/snapshot surface stays on the concrete `Storage`
//! type until/unless other components also want backend pluggability.
//!
//! See [`docs/REFACTOR_STORAGE_TRAIT.md`](../../docs/REFACTOR_STORAGE_TRAIT.md)
//! for the wider plan; this file ships Phase A only (trait + redb impl +
//! in-memory impl). #31 tracks the rest.

use crate::core::block::Block;
use crate::storage::StorageError;

/// Block-level persistence surface used by `BlockStoreCursor`.
///
/// Any type that can durably hold blocks indexed by height, supports
/// pruning, and reports the head height satisfies this trait.
/// `Send + Sync` because the cursor lives inside `Arc<Mutex<Blockchain>>`
/// and crosses tokio task boundaries.
pub trait BlockBackend: Send + Sync {
    /// Persist `block` at `block.header.height`. Overwrite is allowed
    /// (reorg path uses it). Returns Ok on success.
    fn put_block(&self, block: &Block) -> Result<(), StorageError>;

    /// Read the block at `height`, or Ok(None) if absent (pruned or
    /// never seen).
    fn get_block(&self, height: u64) -> Result<Option<Block>, StorageError>;

    /// Drop every block with `height < retain_from`. Returns the count
    /// actually removed. Pruning is a no-op below the current floor.
    fn prune_blocks_below(&self, retain_from: u64) -> Result<usize, StorageError>;

    /// Return the highest known block height (= head). None means storage
    /// has never persisted a block.
    fn get_height(&self) -> Result<Option<u64>, StorageError>;

    /// Flush in-flight writes (no-op for backends that commit synchronously).
    fn flush(&self) -> Result<(), StorageError>;
}

// ─── redb-backed impl ─────────────────────────────────────────────────

impl BlockBackend for super::Storage {
    fn put_block(&self, block: &Block) -> Result<(), StorageError> {
        super::Storage::put_block(self, block)
    }
    fn get_block(&self, height: u64) -> Result<Option<Block>, StorageError> {
        super::Storage::get_block(self, height)
    }
    fn prune_blocks_below(&self, retain_from: u64) -> Result<usize, StorageError> {
        super::Storage::prune_blocks_below(self, retain_from)
    }
    fn get_height(&self) -> Result<Option<u64>, StorageError> {
        super::Storage::get_height(self)
    }
    fn flush(&self) -> Result<(), StorageError> {
        super::Storage::flush(self)
    }
}

// ─── in-memory backend (testing) ──────────────────────────────────────

use std::collections::BTreeMap;
use std::sync::Mutex;

/// In-memory `BlockBackend` — strictly for unit tests. Skips redb +
/// tempdir + fsync, so a suite that uses it runs much faster than one
/// that opens a fresh database per test.
///
/// Behavior matches `Storage::put_block` / `get_block` / `prune_blocks_below`
/// / `get_height` semantics exactly so the cursor tests pass identically
/// against both backends.
#[derive(Default)]
pub struct InMemoryBlockBackend {
    inner: Mutex<BTreeMap<u64, Block>>,
}

impl InMemoryBlockBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlockBackend for InMemoryBlockBackend {
    fn put_block(&self, block: &Block) -> Result<(), StorageError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| StorageError::Io(e.to_string()))?;
        guard.insert(block.header.height, block.clone());
        Ok(())
    }

    fn get_block(&self, height: u64) -> Result<Option<Block>, StorageError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| StorageError::Io(e.to_string()))?;
        Ok(guard.get(&height).cloned())
    }

    fn prune_blocks_below(&self, retain_from: u64) -> Result<usize, StorageError> {
        if retain_from == 0 {
            return Ok(0);
        }
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| StorageError::Io(e.to_string()))?;
        let to_remove: Vec<u64> = guard.range(..retain_from).map(|(h, _)| *h).collect();
        for h in &to_remove {
            guard.remove(h);
        }
        Ok(to_remove.len())
    }

    fn get_height(&self) -> Result<Option<u64>, StorageError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| StorageError::Io(e.to_string()))?;
        Ok(guard.keys().next_back().copied())
    }

    fn flush(&self) -> Result<(), StorageError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::block::Block;

    fn synthetic_block(height: u64) -> Block {
        let mut b = Block::genesis();
        b.header.height = height;
        b.hash = vec![height as u8; 32];
        b
    }

    #[test]
    fn in_memory_put_get_roundtrip() {
        let b = InMemoryBlockBackend::new();
        let block = synthetic_block(5);
        b.put_block(&block).unwrap();
        let read = b.get_block(5).unwrap().unwrap();
        assert_eq!(read.header.height, 5);
        assert_eq!(read.hash, block.hash);
    }

    #[test]
    fn in_memory_get_height_reflects_max() {
        let b = InMemoryBlockBackend::new();
        assert_eq!(b.get_height().unwrap(), None);
        for h in 0..10 {
            b.put_block(&synthetic_block(h)).unwrap();
        }
        assert_eq!(b.get_height().unwrap(), Some(9));
    }

    #[test]
    fn in_memory_prune_below_drops_range() {
        let b = InMemoryBlockBackend::new();
        for h in 0..10 {
            b.put_block(&synthetic_block(h)).unwrap();
        }
        let removed = b.prune_blocks_below(5).unwrap();
        assert_eq!(removed, 5);
        assert!(b.get_block(4).unwrap().is_none());
        assert!(b.get_block(5).unwrap().is_some());
    }

    #[test]
    fn in_memory_prune_zero_is_noop() {
        let b = InMemoryBlockBackend::new();
        for h in 0..3 {
            b.put_block(&synthetic_block(h)).unwrap();
        }
        let removed = b.prune_blocks_below(0).unwrap();
        assert_eq!(removed, 0);
        for h in 0..3 {
            assert!(b.get_block(h).unwrap().is_some());
        }
    }

    #[test]
    fn in_memory_overwrite_same_height() {
        // Reorgs replace blocks at the same height; backend must permit.
        let b = InMemoryBlockBackend::new();
        let mut block_a = synthetic_block(2);
        let mut block_b = synthetic_block(2);
        block_a.hash = vec![0xaa; 32];
        block_b.hash = vec![0xbb; 32];
        b.put_block(&block_a).unwrap();
        b.put_block(&block_b).unwrap();
        let read = b.get_block(2).unwrap().unwrap();
        assert_eq!(read.hash, vec![0xbb; 32]);
    }
}
