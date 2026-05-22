//! Boot-time state replay + canonical-state rebuild.
//!
//! Extracted from `chain/mod.rs` in #29. Three methods that walk the
//! canonical chain block-by-block, applying every transaction onto a
//! projected state to recover the post-tip state from genesis. Used
//! by the boot path (`with_storage_mode` -> `rebuild_canonical_state`)
//! and by snapshot generation (`replay_state_to_canonical_height` -> a
//! point-in-time state for the snapshot's `state_root` claim).

use std::collections::HashMap;

use super::{AccountState, Blockchain, ChainError};
use crate::core::block::Block;
use crate::core::blocktree::BlockTreeError;
use crate::core::receipt::Receipt;
use crate::governance::GovernanceState;
use crate::token::TokenRegistry;
use crate::vm::state::ContractState;

impl Blockchain {
    #[allow(clippy::type_complexity)]
    pub(super) fn replay_state_to_tip(
        &self,
        tip_hash: &[u8],
    ) -> Result<
        (
            HashMap<Vec<u8>, AccountState>,
            HashMap<Vec<u8>, ContractState>,
            HashMap<Vec<u8>, Receipt>,
            TokenRegistry,
            GovernanceState,
        ),
        ChainError,
    > {
        let mut lineage = Vec::new();
        let mut current = tip_hash.to_vec();

        loop {
            let block = self
                .block_tree
                .get(&current)
                .ok_or(BlockTreeError::OrphanBlock)?
                .clone();
            lineage.push(block);
            if current == self.genesis_hash() {
                break;
            }
            current = lineage
                .last()
                .expect("lineage has current block")
                .header
                .prev_hash
                .clone();
        }

        lineage.reverse();

        let mut accounts = Self::accounts_from_genesis(&self.genesis_config)?;
        let mut contracts = HashMap::new();
        let mut receipts = HashMap::new();
        let mut token_registry = TokenRegistry::new();
        let mut governance = GovernanceState::new();
        let mut previous = lineage
            .first()
            .cloned()
            .expect("lineage always includes genesis");
        // Track missed-epochs locally — same reasoning as in
        // `rebuild_canonical_state`: keep this replay path in lockstep with
        // the live `add_block` path so the recomputed state root agrees.
        let mut missed_epochs: HashMap<Vec<u8>, u64> = HashMap::new();
        for block in lineage.iter().skip(1) {
            let producers = self.proposer_addresses_for_settling_epoch(block.header.height);
            Self::apply_epoch_settlement_for_block(
                block.header.height,
                self.epoch_length,
                &self.epoch_snapshots,
                &producers,
                &mut accounts,
                &mut missed_epochs,
            );
            let execution = self.validate_block_against_state(
                block,
                &previous,
                &accounts,
                &contracts,
                &token_registry,
                &governance,
            )?;
            accounts = execution.accounts;
            contracts = execution.contracts;
            receipts.extend(execution.receipts);
            token_registry = execution.token_registry;
            governance = execution.governance;
            previous = block.clone();
        }

        Ok((accounts, contracts, receipts, token_registry, governance))
    }

    #[allow(clippy::type_complexity)]
    pub(super) fn replay_state_to_canonical_height(
        &self,
        target_height: u64,
    ) -> Result<
        (
            HashMap<Vec<u8>, AccountState>,
            HashMap<Vec<u8>, ContractState>,
            HashMap<Vec<u8>, Receipt>,
            TokenRegistry,
            GovernanceState,
        ),
        ChainError,
    > {
        let block = self
            .block_at_height(target_height)
            .ok_or_else(|| ChainError::SnapshotError("target height missing".to_string()))?;
        self.replay_state_to_tip(&block.hash)
    }

    pub(super) fn rebuild_canonical_state(&mut self) -> Result<(), ChainError> {
        let blocks: Vec<Block> = self.iter_blocks().collect();
        let mut accounts = Self::accounts_from_genesis(&self.genesis_config)?;
        let mut contracts = HashMap::new();
        let mut receipts = HashMap::new();
        let mut token_registry = TokenRegistry::new();
        let mut governance = GovernanceState::new();
        self.epoch_snapshots.clear();
        self.epoch_snapshots
            .insert(0, self.snapshot_for_accounts(0, &accounts));
        // Rebuild the in-memory hash → height / tx → location indexes from the
        // canonical chain. These were lost on restart and the alternative was
        // an O(n) scan on every lookup.
        self.block_hash_to_height.clear();
        self.tx_hash_index.clear();
        self.evm_tx_hash_index.clear();
        for block in &blocks {
            self.block_hash_to_height
                .insert(block.hash.clone(), block.header.height);
            for (tx_index, tx) in block.transactions.iter().enumerate() {
                self.tx_hash_index
                    .insert(tx.hash(), (block.header.height, tx_index));
                if tx.is_evm()
                    && let Ok(decoded) = crate::vm::evm::decode_raw_eth_tx(&tx.evm_raw_tx)
                {
                    self.evm_tx_hash_index
                        .insert(decoded.tx_hash.to_vec(), (block.header.height, tx_index));
                }
            }
        }

        let mut previous = blocks
            .first()
            .cloned()
            .ok_or_else(|| ChainError::InvalidGenesis("missing genesis block".to_string()))?;
        // Mirror `add_block`'s missed-epochs tracker so that any settlement
        // beyond the first epoch sees the same accumulated misses it would
        // see in the live path. Persisted state is not affected by
        // `validator_missed_epochs` directly, but it influences the
        // inactivity-penalty branch of `compute_epoch_settlement` which
        // subtracts from `staked_balance`. Drift here was a contributing
        // factor to long-tail state-root mismatches, in addition to the
        // primary reward-distribution bug.
        let mut missed_epochs: HashMap<Vec<u8>, u64> = HashMap::new();
        for block in blocks.iter().skip(1) {
            if block.header.height > 0
                && block.header.height.is_multiple_of(self.epoch_length.max(1))
            {
                let epoch = self.epoch_for_height(block.header.height);
                if !self.epoch_snapshots.contains_key(&epoch) {
                    self.create_epoch_snapshot_from_accounts(epoch, &accounts);
                }
            }

            // Apply epoch settlement on the parent accounts in lockstep with
            // `add_block`. Without this the recomputed state root for the
            // boundary block diverges (the rewards minted in the live path
            // are missing here) and `validate_block_against_state` returns
            // `InvalidStateRoot`, which `with_storage` surfaces as the
            // `Failed to initialize blockchain storage: invalid state root`
            // crash loop seen on the testnet at every multiple of
            // `epoch_length` past `2 * epoch_length`.
            let producers = self.proposer_addresses_for_settling_epoch(block.header.height);
            Self::apply_epoch_settlement_for_block(
                block.header.height,
                self.epoch_length,
                &self.epoch_snapshots,
                &producers,
                &mut accounts,
                &mut missed_epochs,
            );

            let execution = self.validate_block_against_state(
                block,
                &previous,
                &accounts,
                &contracts,
                &token_registry,
                &governance,
            )?;
            accounts = execution.accounts;
            contracts = execution.contracts;
            receipts.extend(execution.receipts);
            token_registry = execution.token_registry;
            governance = execution.governance;
            previous = block.clone();
        }

        self.accounts = accounts;
        self.contracts = contracts;
        self.receipts = receipts;
        self.token_registry = token_registry;
        self.governance = governance;
        self.validator_missed_epochs = missed_epochs;
        self.rebuild_receipt_indexes();
        Ok(())
    }
}
