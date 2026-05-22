//! Block production. Lifted from `chain/mod.rs` in #29.
//!
//! `create_block` is `&self` — it runs the slot-leader gate, projects
//! the next state by applying every admissible pending transaction
//! into clones of `accounts` / `contracts` / `token_registry` /
//! `governance` / `receipts`, computes the state root for the
//! protocol version at the target height, then signs the block.
//! Callers (network production loop, tests) get an owned `Block`
//! they can broadcast or feed to `add_block` for adoption.

use std::collections::{HashMap, HashSet};

use super::{Blockchain, ChainError};
use crate::core::block::Block;
use crate::core::transaction::Transaction;
use crate::crypto::dilithium::KeyPair;
use crate::crypto::hash;

impl Blockchain {
    pub fn create_block(&self, validator_keypair: &KeyPair) -> Result<Block, ChainError> {
        let prev_block = self.latest_block();
        let height = prev_block.header.height + 1;
        let prev_hash = prev_block.hash.clone();
        let protocol_version = self.protocol_version_at_height(height);
        let base_fee_per_gas = self.next_base_fee_per_gas(&prev_block);

        let proposer_public_key = validator_keypair.public_key.clone();
        let proposer_address = hash::address_bytes_from_public_key(&proposer_public_key);
        // Production-side leader check: derive the allowed backup rank from
        // wall-clock vs. parent timestamp, so a validator that's woken up
        // late (because the primary is offline) can still produce when
        // legitimately authorized as a backup.
        let now = chrono::Utc::now().timestamp();
        let snapshot_size = self
            .snapshot_for_height(&self.accounts, &self.epoch_snapshots, height)
            .map(|s| s.validators.len())
            .unwrap_or(0);
        let allowed_rank = Self::allowed_backup_rank_for_height(
            height,
            prev_block.header.timestamp,
            now,
            snapshot_size,
        );
        self.ensure_validator_is_authorized_for_accounts_at_rank(
            &self.accounts,
            &self.epoch_snapshots,
            &proposer_public_key,
            height,
            &prev_hash,
            allowed_rank,
        )?;

        let mut projected_accounts = self.accounts.clone();
        let mut projected_contracts = self.contracts.clone();
        let mut projected_receipts = HashMap::new();
        let mut projected_token_registry = self.token_registry.clone();
        let mut projected_governance = self.governance.clone();
        Self::apply_unstake_unlocks(&mut projected_accounts, height);

        // Apply epoch settlement if crossing epoch boundary. We feed a *clone*
        // of `self.validator_missed_epochs` to the helper because
        // `create_block` is `&self` and the canonical update of the tracker
        // happens in `add_block` once the block is actually accepted.
        let mut projected_missed = self.validator_missed_epochs.clone();
        let producers = self.proposer_addresses_for_settling_epoch(height);
        Self::apply_epoch_settlement_for_block(
            height,
            self.epoch_length,
            &self.epoch_snapshots,
            &producers,
            &mut projected_accounts,
            &mut projected_missed,
        );
        let mut block_txs = Vec::new();
        let mut total_priority_fees = 0u64;
        let mut total_gas_used = 0u64;
        let mut seen_hashes = HashSet::new();

        for pending in &self.pending_transactions {
            let tx_hash = pending.hash();
            if !seen_hashes.insert(tx_hash) {
                continue;
            }

            match Self::apply_user_transaction(
                &mut projected_accounts,
                &mut projected_contracts,
                &mut projected_receipts,
                &mut projected_token_registry,
                &mut projected_governance,
                pending,
                height,
                self.unstake_delay_blocks,
                self.epoch_length,
                self.minimum_stake,
                base_fee_per_gas,
            ) {
                Ok(gas_used) if total_gas_used.saturating_add(gas_used) <= self.block_gas_limit => {
                    total_gas_used = total_gas_used.saturating_add(gas_used);
                    total_priority_fees = total_priority_fees.saturating_add(
                        Self::priority_fee_for_transaction(pending, gas_used, base_fee_per_gas),
                    );
                    block_txs.push(pending.clone());
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }

        let coinbase = Transaction::coinbase(
            &self.genesis_config.chain_id,
            proposer_address.clone(),
            self.block_reward.saturating_add(total_priority_fees),
        );
        Self::apply_coinbase_transaction(&mut projected_accounts, &coinbase)?;

        let mut transactions = vec![coinbase];
        transactions.extend(block_txs);

        // State root must use the protocol version corresponding to
        // the height of the block being produced. At v5 baseline this
        // is identical to the prior `compute_state_root_full` call.
        // After the v6 hardfork (gated by `V6_HARDFORK_HEIGHT_TESTNET`)
        // becomes active, this is the SMT root.
        let state_root = Self::compute_state_root_at_protocol(
            &projected_accounts,
            &projected_contracts,
            protocol_version,
        );
        Ok(Block::new(
            protocol_version,
            height,
            prev_hash,
            state_root,
            total_gas_used,
            base_fee_per_gas,
            transactions,
            validator_keypair,
        ))
    }
}
