use std::collections::HashMap;

use crate::core::block::Block;

/// A node in the block tree, storing a block and its relationship to other blocks.
pub struct BlockEntry {
    pub block: Block,
    /// Sum of proposer stakes along the chain from genesis to this block
    pub cumulative_stake_weight: u64,
    /// Hashes of blocks that build on top of this one
    pub children: Vec<Vec<u8>>,
}

/// Tree of blocks supporting forks, with a canonical tip and finality boundary.
///
/// The fork choice rule selects the chain tip with the highest cumulative
/// proposer-stake weight. Finalized blocks cannot be reverted.
pub struct BlockTree {
    entries: HashMap<Vec<u8>, BlockEntry>,
    /// Hash of the current best (canonical) chain tip
    canonical_tip: Vec<u8>,
    /// Hash of the latest finalized block
    pub finalized_hash: Vec<u8>,
    /// Height of the latest finalized block
    pub finalized_height: u64,
}

impl BlockTree {
    /// Create a new block tree rooted at the genesis block.
    pub fn from_genesis(genesis: &Block) -> Self {
        let genesis_hash = genesis.hash.clone();
        let mut entries = HashMap::new();
        entries.insert(
            genesis_hash.clone(),
            BlockEntry {
                block: genesis.clone(),
                cumulative_stake_weight: 0,
                children: Vec::new(),
            },
        );

        BlockTree {
            entries,
            canonical_tip: genesis_hash.clone(),
            finalized_hash: genesis_hash,
            finalized_height: 0,
        }
    }

    /// Insert a block into the tree. The block's prev_hash must already exist.
    /// `proposer_stake` is the stake weight of the block's proposer.
    /// Returns true if the canonical tip changed (reorg needed).
    pub fn insert(&mut self, block: Block, proposer_stake: u64) -> Result<bool, BlockTreeError> {
        let block_hash = block.hash.clone();
        let prev_hash = block.header.prev_hash.clone();

        if self.entries.contains_key(&block_hash) {
            return Ok(false); // Already known
        }

        // Reject blocks that would fork below finalized height
        if block.header.height <= self.finalized_height && block_hash != self.finalized_hash {
            return Err(BlockTreeError::BelowFinalized);
        }

        let parent_weight = self
            .entries
            .get(&prev_hash)
            .ok_or(BlockTreeError::OrphanBlock)?
            .cumulative_stake_weight;

        let cumulative_weight = parent_weight + proposer_stake;

        // Add as child of parent
        if let Some(parent) = self.entries.get_mut(&prev_hash) {
            parent.children.push(block_hash.clone());
        }

        self.entries.insert(
            block_hash.clone(),
            BlockEntry {
                block,
                cumulative_stake_weight: cumulative_weight,
                children: Vec::new(),
            },
        );

        // Check if this new block is now the heaviest tip
        let old_tip_weight = self
            .entries
            .get(&self.canonical_tip)
            .map(|e| e.cumulative_stake_weight)
            .unwrap_or(0);

        if cumulative_weight > old_tip_weight {
            let old_tip = self.canonical_tip.clone();
            self.canonical_tip = block_hash;
            Ok(old_tip != self.canonical_tip_parent_chain_includes(&old_tip))
        } else {
            Ok(false)
        }
    }

    /// Helper: check if old_tip is an ancestor of the new canonical tip.
    /// If yes, no reorg needed (just extension). If no, reorg needed.
    fn canonical_tip_parent_chain_includes(&self, old_tip: &[u8]) -> Vec<u8> {
        // Walk back from canonical_tip to see if old_tip is an ancestor
        let mut current = self.canonical_tip.clone();
        while let Some(entry) = self.entries.get(&current) {
            if current == old_tip {
                return current; // Old tip is ancestor, return it to signal no reorg
            }
            if entry.block.header.height == 0 {
                break;
            }
            current = entry.block.header.prev_hash.clone();
        }
        Vec::new() // Old tip is not an ancestor — reorg needed
    }

    /// Get the canonical chain tip hash
    pub fn canonical_tip(&self) -> &[u8] {
        &self.canonical_tip
    }

    /// Get the canonical chain as a sequence of blocks from genesis to tip
    pub fn canonical_chain(&self) -> Vec<&Block> {
        let mut chain = Vec::new();
        let mut current = self.canonical_tip.clone();
        while let Some(entry) = self.entries.get(&current) {
            chain.push(&entry.block);
            if entry.block.header.height == 0 {
                break;
            }
            current = entry.block.header.prev_hash.clone();
        }
        chain.reverse();
        chain
    }

    /// Find the common ancestor between two block hashes
    pub fn common_ancestor(&self, hash_a: &[u8], hash_b: &[u8]) -> Option<Vec<u8>> {
        // Collect ancestors of hash_a
        let mut ancestors_a = std::collections::HashSet::new();
        let mut current = hash_a.to_vec();
        while let Some(entry) = self.entries.get(&current) {
            ancestors_a.insert(current.clone());
            if entry.block.header.height == 0 {
                break;
            }
            current = entry.block.header.prev_hash.clone();
        }

        // Walk back from hash_b until we find a common ancestor
        let mut current = hash_b.to_vec();
        while let Some(entry) = self.entries.get(&current) {
            if ancestors_a.contains(&current) {
                return Some(current);
            }
            if entry.block.header.height == 0 {
                break;
            }
            current = entry.block.header.prev_hash.clone();
        }

        None
    }

    /// Get blocks from ancestor (exclusive) to descendant (inclusive)
    pub fn chain_between(&self, ancestor_hash: &[u8], descendant_hash: &[u8]) -> Vec<&Block> {
        let mut blocks = Vec::new();
        let mut current = descendant_hash.to_vec();
        while let Some(entry) = self.entries.get(&current) {
            if current == ancestor_hash {
                break;
            }
            blocks.push(&entry.block);
            if entry.block.header.height == 0 {
                break;
            }
            current = entry.block.header.prev_hash.clone();
        }
        blocks.reverse();
        blocks
    }

    /// Mark a block as finalized. All blocks on non-canonical branches
    /// at or below this height can be pruned.
    pub fn set_finalized(&mut self, hash: Vec<u8>, height: u64) {
        self.finalized_hash = hash;
        self.finalized_height = height;
        self.prune_below_finalized();
    }

    /// Remove entries that are not on the canonical chain and are at or below
    /// the finalized height.
    fn prune_below_finalized(&mut self) {
        if self.finalized_height == 0 {
            return;
        }

        // Collect canonical chain hashes
        let canonical_hashes: std::collections::HashSet<Vec<u8>> = self
            .canonical_chain()
            .iter()
            .map(|b| b.hash.clone())
            .collect();

        // Remove non-canonical entries at or below finalized height
        self.entries.retain(|hash, entry| {
            if entry.block.header.height <= self.finalized_height {
                canonical_hashes.contains(hash)
            } else {
                true
            }
        });
    }

    /// Check if a block hash is known to the tree
    pub fn contains(&self, hash: &[u8]) -> bool {
        self.entries.contains_key(hash)
    }

    /// Get a block by hash
    pub fn get(&self, hash: &[u8]) -> Option<&Block> {
        self.entries.get(hash).map(|e| &e.block)
    }

    /// Total number of blocks in the tree (including forks)
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlockTreeError {
    #[error("block's parent is not in the tree")]
    OrphanBlock,
    #[error("block would fork below finalized height")]
    BelowFinalized,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::block::Block;
    use crate::core::transaction::Transaction;
    use crate::crypto::dilithium::KeyPair;
    use crate::crypto::hash;

    fn make_block(height: u64, prev_hash: Vec<u8>, kp: &KeyPair) -> Block {
        let coinbase = Transaction::coinbase(vec![1; hash::ADDRESS_LEN], 50);
        Block::new(
            height,
            prev_hash,
            hash::sha3_hash(b"state"),
            vec![coinbase],
            kp,
        )
    }

    #[test]
    fn test_block_tree_basic() {
        let genesis = Block::genesis();
        let kp = KeyPair::generate();
        let mut tree = BlockTree::from_genesis(&genesis);

        let block1 = make_block(1, genesis.hash.clone(), &kp);
        let _changed = tree.insert(block1.clone(), 5000).unwrap();
        assert_eq!(tree.canonical_chain().len(), 2);
    }

    #[test]
    fn test_fork_choice_heaviest_wins() {
        let genesis = Block::genesis();
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let mut tree = BlockTree::from_genesis(&genesis);

        // Branch A: one block with stake 1000
        let block_a = make_block(1, genesis.hash.clone(), &kp_a);
        tree.insert(block_a.clone(), 1_000).unwrap();

        // Branch B: one block with stake 5000 (heavier)
        let block_b = make_block(1, genesis.hash.clone(), &kp_b);
        tree.insert(block_b.clone(), 5_000).unwrap();

        // Canonical tip should be block_b (heavier)
        assert_eq!(tree.canonical_tip(), block_b.hash.as_slice());
    }

    #[test]
    fn test_common_ancestor() {
        let genesis = Block::genesis();
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let mut tree = BlockTree::from_genesis(&genesis);

        let block_a = make_block(1, genesis.hash.clone(), &kp_a);
        tree.insert(block_a.clone(), 1000).unwrap();

        let block_b = make_block(1, genesis.hash.clone(), &kp_b);
        tree.insert(block_b.clone(), 1000).unwrap();

        let ancestor = tree.common_ancestor(&block_a.hash, &block_b.hash);
        assert!(ancestor.is_some());
        // Both blocks have genesis as parent, so common ancestor is genesis
        let ancestor_hash = ancestor.unwrap();
        assert_eq!(ancestor_hash, genesis.hash);
    }

    #[test]
    fn test_reject_below_finalized() {
        let genesis = Block::genesis();
        let kp = KeyPair::generate();
        let mut tree = BlockTree::from_genesis(&genesis);

        let block1 = make_block(1, genesis.hash.clone(), &kp);
        tree.insert(block1.clone(), 1000).unwrap();

        let block2 = make_block(2, block1.hash.clone(), &kp);
        tree.insert(block2.clone(), 1000).unwrap();

        // Finalize at height 2
        tree.set_finalized(block2.hash.clone(), 2);

        // Try to insert a competing block at height 1 (below finalized)
        let kp2 = KeyPair::generate();
        let fork = make_block(1, genesis.hash.clone(), &kp2);
        let result = tree.insert(fork, 5000);
        assert!(matches!(result, Err(BlockTreeError::BelowFinalized)));
    }

    #[test]
    fn test_pruning() {
        let genesis = Block::genesis();
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let mut tree = BlockTree::from_genesis(&genesis);

        let block_a = make_block(1, genesis.hash.clone(), &kp_a);
        tree.insert(block_a.clone(), 5000).unwrap();

        let block_b = make_block(1, genesis.hash.clone(), &kp_b);
        tree.insert(block_b.clone(), 1000).unwrap();

        // block_a is canonical tip (heaviest)
        assert_eq!(tree.canonical_tip(), block_a.hash.as_slice());
        assert_eq!(tree.len(), 3); // genesis + 2 forks

        // Extend canonical chain so we can finalize beyond the fork
        let block_a2 = make_block(2, block_a.hash.clone(), &kp_a);
        tree.insert(block_a2.clone(), 5000).unwrap();

        // Finalize at height 2 — block_b (height 1, non-canonical) should be pruned
        tree.set_finalized(block_a2.hash.clone(), 2);

        assert!(!tree.contains(&block_b.hash));
        // genesis + block_a + block_a2 remain
        assert_eq!(tree.len(), 3);
    }
}
