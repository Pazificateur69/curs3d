//! Fork choice + chain reorg path. Extracted from `chain/mod.rs` in
//! #29.
//!
//! `add_block_with_fork_choice` is the network-facing entry: when a
//! block arrives that doesn't build on our current tip, route it
//! here. Internally:
//! - if it builds on the tip, just delegate to `add_block` (no reorg)
//! - otherwise replay state to the new parent, validate the block,
//!   insert into `block_tree`, and if the heavier-chain rule now
//!   picks a different tip, walk that side and replace canonical
//!   state.
//!
//! `reorg_to_canonical_tip` is the helper that performs the actual
//! switch — guarded by `ReorgBelowFinality` so finalized history
//! cannot be rewritten.

use super::{Blockchain, ChainError};
use crate::core::block::Block;
use crate::core::blocktree::BlockTreeError;
use crate::crypto::hash;

impl Blockchain {
    pub fn add_block_with_fork_choice(&mut self, block: Block) -> Result<bool, ChainError> {
        // Reject blocks too far behind the chain tip to limit reorg depth
        const MAX_REORG_DEPTH: u64 = 64;
        if self.height() > MAX_REORG_DEPTH
            && block.header.height < self.height().saturating_sub(MAX_REORG_DEPTH)
        {
            return Err(ChainError::InvalidTransactionFormat(
                "block too old: exceeds maximum reorg depth",
            ));
        }

        let builds_on_tip = block.header.prev_hash == self.latest_block().hash;

        if builds_on_tip {
            self.add_block(block)?;
            return Ok(false); // No reorg
        }

        let parent = self
            .block_tree
            .get(&block.header.prev_hash)
            .ok_or(BlockTreeError::OrphanBlock)?
            .clone();
        let (parent_accounts, parent_contracts, _, parent_token_registry, parent_governance) =
            self.replay_state_to_tip(&parent.hash)?;
        let execution = self.validate_block_against_state(
            &block,
            &parent,
            &parent_accounts,
            &parent_contracts,
            &parent_token_registry,
            &parent_governance,
        )?;

        // Get proposer stake for weight calculation
        let proposer_address =
            hash::address_bytes_from_public_key(&block.header.validator_public_key);
        let proposer_stake = execution
            .accounts
            .get(&proposer_address)
            .map(|a| a.staked_balance)
            .unwrap_or(0);

        // Insert into block tree
        let tip_changed = self.block_tree.insert(block.clone(), proposer_stake)?;

        if tip_changed {
            // The fork is now heavier — perform reorg
            tracing::warn!(
                "Fork detected at height {}. Reorg triggered.",
                block.header.height
            );
            self.reorg_to_canonical_tip()?;
            Ok(true) // Reorg happened
        } else {
            tracing::info!(
                "Fork block at height {} stored but canonical tip unchanged.",
                block.header.height
            );
            Ok(false)
        }
    }

    /// Replay the canonical chain from the block tree, rebuilding accounts.
    pub(super) fn reorg_to_canonical_tip(&mut self) -> Result<(), ChainError> {
        let canonical = self.block_tree.canonical_chain();
        let canonical_tip_height = canonical.last().map(|b| b.header.height).unwrap_or(0);

        // Cannot reorg below finalized height
        if canonical_tip_height < self.finality_tracker.finalized_height {
            return Err(ChainError::ReorgBelowFinality(
                self.finality_tracker.finalized_height,
            ));
        }

        if self.finality_tracker.finalized_height > 0 {
            let current_tip = self.latest_block().hash.clone();
            let new_tip = canonical
                .last()
                .map(|b| b.hash.clone())
                .unwrap_or_else(|| self.genesis_hash().to_vec());
            let ancestor = self
                .block_tree
                .common_ancestor(&current_tip, &new_tip)
                .ok_or(ChainError::InvalidPrevHash)?;
            let ancestor_block = self
                .block_tree
                .get(&ancestor)
                .ok_or(ChainError::InvalidPrevHash)?;
            if ancestor_block.header.height < self.finality_tracker.finalized_height {
                return Err(ChainError::ReorgBelowFinality(
                    self.finality_tracker.finalized_height,
                ));
            }
            if !self
                .block_tree
                .is_descendant_of(&new_tip, &self.finality_tracker.finalized_hash)
            {
                return Err(ChainError::ReorgBelowFinality(
                    self.finality_tracker.finalized_height,
                ));
            }
        }

        self.replace_all_blocks(canonical.iter().cloned().cloned().collect());
        self.rebuild_canonical_state()?;
        self.persist_full_state()?;

        tracing::info!(
            "Reorg complete. New height: {}, new tip: {}",
            self.height(),
            self.latest_block().hash_hex()
        );

        Ok(())
    }
}
