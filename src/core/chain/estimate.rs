//! Dry-run transaction estimation. Pure projection of the next block's
//! state with `tx` admitted alongside the existing mempool — same
//! ordering, same apply path. Returns gas + effective fee + refund as
//! a `TransactionEstimate` for the API (`POST /api/tx/estimate`).
//!
//! Extracted from `chain/mod.rs` in #29.

use std::collections::{HashMap, HashSet};

use super::{Blockchain, ChainError, TransactionEstimate};
use crate::core::transaction::Transaction;

impl Blockchain {
    pub fn estimate_transaction(
        &self,
        tx: &Transaction,
    ) -> Result<TransactionEstimate, ChainError> {
        if tx.is_coinbase() {
            return Err(ChainError::InvalidTransactionFormat(
                "coinbase transactions cannot be estimated",
            ));
        }
        if tx.chain_id != self.genesis_config.chain_id {
            return Err(ChainError::InvalidChainId {
                expected: self.genesis_config.chain_id.clone(),
                got: tx.chain_id.clone(),
            });
        }

        let pending_base_fee = self.next_base_fee_per_gas(&self.latest_block());
        let replacement_index = self
            .pending_transactions
            .iter()
            .position(|pending| pending.from == tx.from && pending.nonce == tx.nonce);

        let mut projected_accounts = self.accounts.clone();
        let mut projected_contracts = self.contracts.clone();
        let mut projected_receipts = HashMap::new();
        let mut projected_token_registry = self.token_registry.clone();
        let mut projected_governance = self.governance.clone();
        let mut seen_hashes = HashSet::new();

        for (index, pending) in self.pending_transactions.iter().enumerate() {
            if replacement_index == Some(index) {
                continue;
            }
            let pending_hash = pending.hash();
            if !seen_hashes.insert(pending_hash) {
                continue;
            }
            Self::apply_user_transaction(
                &mut projected_accounts,
                &mut projected_contracts,
                &mut projected_receipts,
                &mut projected_token_registry,
                &mut projected_governance,
                pending,
                self.height() + 1,
                self.unstake_delay_blocks,
                self.epoch_length,
                self.minimum_stake,
                pending_base_fee,
            )?;
        }

        let gas_used = Self::apply_user_transaction(
            &mut projected_accounts,
            &mut projected_contracts,
            &mut projected_receipts,
            &mut projected_token_registry,
            &mut projected_governance,
            tx,
            self.height() + 1,
            self.unstake_delay_blocks,
            self.epoch_length,
            self.minimum_stake,
            pending_base_fee,
        )?;
        let effective_gas_price = tx
            .effective_gas_price(pending_base_fee)
            .ok_or(ChainError::FeeTooLow)?;
        let total_fee_charged = gas_used.saturating_mul(effective_gas_price);
        let gas_refunded = tx.total_fee_cap().saturating_sub(total_fee_charged);
        let priority_fee_paid = Self::priority_fee_for_transaction(tx, gas_used, pending_base_fee);
        let base_fee_burned = gas_used.saturating_mul(pending_base_fee);

        Ok(TransactionEstimate {
            next_block_height: self.height() + 1,
            base_fee_per_gas: pending_base_fee,
            gas_used,
            effective_gas_price,
            priority_fee_paid,
            base_fee_burned,
            total_fee_charged,
            gas_refunded,
            max_total_fee: tx.total_fee_cap(),
            would_replace_pending: replacement_index.is_some(),
        })
    }
}
