//! Block application entry point + per-block validation.
//!
//! Extracted from `chain/mod.rs` in #29. This is the *critical path*
//! of the chain — every block accepted by this node lands here. The
//! split brings two methods plus their projected-state container:
//!
//! - [`Blockchain::add_block`] — pub entry: epoch settlement,
//!   validate, hardcoded-checkpoint enforcement, commit the
//!   projected state, push to block tree + cursor, rebuild indexes,
//!   prune mempool, persist.
//! - [`Blockchain::validate_block_against_state`] —
//!   `pub(super)` helper: full structural + signature + protocol +
//!   slot-leader + per-tx + state-root verification against the parent
//!   state passed in. Returns the projected [`BlockExecution`] on
//!   success.
//!
//! The per-transaction apply path (`apply_user_transaction`,
//! `apply_coinbase_transaction`, `apply_unstake_unlocks`) and the
//! validator-authorization helpers remain in `mod.rs` for now —
//! they're shared with `create_block` (`chain::produce`) and the
//! replay paths (`chain::replay`) and don't have a single owner.

use std::collections::{HashMap, HashSet};

use super::{AccountState, Blockchain, ChainError, MAX_FUTURE_BLOCK_TIME_SECS};
use crate::consensus::EpochSnapshot;
use crate::core::block::Block;
use crate::core::checkpoints;
use crate::core::receipt::Receipt;
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::hash;
use crate::governance::GovernanceState;
use crate::token::TokenRegistry;
use crate::vm::Vm;
use crate::vm::state::ContractState;

/// Projected post-execution state. Returned by
/// [`Blockchain::validate_block_against_state`] and consumed by
/// [`Blockchain::add_block`] (and `replay_state_to_tip`,
/// `rebuild_canonical_state`, the reorg path).
pub(super) struct BlockExecution {
    pub(super) accounts: HashMap<Vec<u8>, AccountState>,
    pub(super) contracts: HashMap<Vec<u8>, ContractState>,
    pub(super) receipts: HashMap<Vec<u8>, Receipt>,
    pub(super) token_registry: TokenRegistry,
    pub(super) governance: GovernanceState,
}

// Silence the unused-import warning when only some of these types
// happen to be touched at this module's level — they're all reachable
// from the impl block below in one form or another.
#[allow(dead_code)]
type _EpochSnapshotAlias = EpochSnapshot;

impl Blockchain {
    pub fn add_block(&mut self, block: Block) -> Result<(), ChainError> {
        let prev_accounts = self.accounts.clone();
        let new_epoch = self.epoch_for_height(block.header.height);

        if block.header.height > 0
            && block.header.height.is_multiple_of(self.epoch_length.max(1))
            && !self.epoch_snapshots.contains_key(&new_epoch)
        {
            self.create_epoch_snapshot_from_accounts(new_epoch, &prev_accounts);
        }

        // Epoch settlement: when crossing epoch boundary, compute and apply
        // rewards/penalties. The same helper is invoked by the boot-time
        // replay path (`rebuild_canonical_state` / `replay_state_to_tip`) so
        // both the apply path and the load path agree on the post-settlement
        // accounts that feed into `validate_block_against_state`. Mismatched
        // settlement application was the root cause of the
        // `state_root_mismatch` crash on restart at multiples of
        // `epoch_length` past `2 * epoch_length`.
        let producers = self.proposer_addresses_for_settling_epoch(block.header.height);
        Self::apply_epoch_settlement_for_block(
            block.header.height,
            self.epoch_length,
            &self.epoch_snapshots,
            &producers,
            &mut self.accounts,
            &mut self.validator_missed_epochs,
        );
        let prev = self.latest_block();
        let execution = self.validate_block_against_state(
            &block,
            &prev,
            &self.accounts,
            &self.contracts,
            &self.token_registry,
            &self.governance,
        )?;

        // Hardcoded-checkpoint enforcement. Runs AFTER state validation
        // (so we don't waste cycles diffing an invalid block) but BEFORE
        // any state mutation (so a rejected block leaves no trace). The
        // checkpoint list is empty for most chains; this is the safety
        // net for `curs3d-public-testnet` and future mainnet anchors.
        checkpoints::verify_block_against_known(self.chain_id(), block.header.height, &block.hash)?;

        // Insert into block tree for fork tracking
        let proposer_address =
            hash::address_bytes_from_public_key(&block.header.validator_public_key);
        let proposer_stake = execution
            .accounts
            .get(&proposer_address)
            .map(|a| a.staked_balance)
            .unwrap_or(0);
        // Ignore block tree errors for blocks already in the tree
        let _ = self.block_tree.insert(block.clone(), proposer_stake);

        self.accounts = execution.accounts;
        self.contracts = execution.contracts;
        self.receipts.extend(execution.receipts);
        self.token_registry = execution.token_registry;
        self.governance = execution.governance;
        self.push_block_internal(block.clone());
        self.block_hash_to_height
            .insert(block.hash.clone(), block.header.height);
        for (tx_index, tx) in block.transactions.iter().enumerate() {
            self.tx_hash_index
                .insert(tx.hash(), (block.header.height, tx_index));
            // Also index EVM txs under their Ethereum-shape hash so MetaMask /
            // forge / ethers.js can look them up using the hash they computed
            // client-side (keccak256 over the RLP-signed payload).
            if tx.is_evm()
                && let Ok(decoded) = crate::vm::evm::decode_raw_eth_tx(&tx.evm_raw_tx)
            {
                self.evm_tx_hash_index
                    .insert(decoded.tx_hash.to_vec(), (block.header.height, tx_index));
            }
        }
        self.rebuild_receipt_indexes();
        self.remove_block_transactions_from_mempool(&block);
        // In live node mode, sled writes are handled by a bounded background
        // worker. gdb traces from the May 2026 soak showed sled 0.34 can park
        // its IO workers on an internal mutex while `add_block` is holding the
        // chain mutex. Keeping all sled calls out of this critical path means
        // storage can stall without freezing consensus, RPC, or gossipsub.
        // The synchronous branch remains for deterministic unit tests and
        // offline CLI tooling.
        self.persist_added_block(&block)?;
        let height = block.header.height;
        let is_epoch_boundary = self.epoch_length > 0 && height.is_multiple_of(self.epoch_length);
        if is_epoch_boundary {
            self.persist_full_state()?;
        }
        tracing::info!(
            target: "audit",
            event = "block_added",
            height = block.header.height,
            tx_count = block.transactions.len(),
            hash = %hex::encode(&block.hash),
        );

        Ok(())
    }

    pub(super) fn validate_block_against_state(
        &self,
        block: &Block,
        parent: &Block,
        parent_accounts: &HashMap<Vec<u8>, AccountState>,
        parent_contracts: &HashMap<Vec<u8>, ContractState>,
        parent_token_registry: &TokenRegistry,
        parent_governance: &GovernanceState,
    ) -> Result<BlockExecution, ChainError> {
        if block.header.height != parent.header.height + 1 {
            return Err(ChainError::InvalidHeight {
                expected: parent.header.height + 1,
                got: block.header.height,
            });
        }

        if block.header.prev_hash != parent.hash {
            return Err(ChainError::InvalidPrevHash);
        }

        if !block.verify_hash() {
            return Err(ChainError::InvalidBlockHash);
        }

        if !block.verify_merkle_root() {
            return Err(ChainError::InvalidMerkleRoot);
        }

        if !block.verify_signature() {
            return Err(ChainError::InvalidBlockSignature);
        }

        let now = chrono::Utc::now().timestamp();
        let min_timestamp = parent.header.timestamp;
        let max_timestamp = now + MAX_FUTURE_BLOCK_TIME_SECS;
        if block.header.timestamp < min_timestamp || block.header.timestamp > max_timestamp {
            return Err(ChainError::InvalidBlockTimestamp {
                got: block.header.timestamp,
                min: min_timestamp,
                max: max_timestamp,
            });
        }

        // Check protocol version matches expected version for this height
        let expected_version = self.protocol_version_at_height(block.header.height);
        if block.header.version != expected_version {
            return Err(ChainError::InvalidProtocolVersion {
                expected: expected_version,
                got: block.header.version,
            });
        }
        let expected_base_fee = self.next_base_fee_per_gas(parent);
        if block.header.base_fee_per_gas != expected_base_fee {
            return Err(ChainError::InvalidBaseFee {
                expected: expected_base_fee,
                got: block.header.base_fee_per_gas,
            });
        }

        // Slot-leader scheduling: the producer must be either the
        // primary leader for this height, or a backup whose rank is
        // justified by the elapsed time since the parent block.
        let snapshot_size = self
            .snapshot_for_height(parent_accounts, &self.epoch_snapshots, block.header.height)
            .map(|s| s.validators.len())
            .unwrap_or(0);
        let allowed_rank = Self::allowed_backup_rank_for_height(
            block.header.height,
            parent.header.timestamp,
            block.header.timestamp,
            snapshot_size,
        );
        self.ensure_validator_is_authorized_for_accounts_at_rank(
            parent_accounts,
            &self.epoch_snapshots,
            &block.header.validator_public_key,
            block.header.height,
            &block.header.prev_hash,
            allowed_rank,
        )?;

        let proposer_address =
            hash::address_bytes_from_public_key(&block.header.validator_public_key);
        let mut projected_accounts = parent_accounts.clone();
        let mut projected_contracts = parent_contracts.clone();
        let mut projected_receipts = HashMap::new();
        let mut projected_token_registry = parent_token_registry.clone();
        let mut projected_governance = parent_governance.clone();
        Self::apply_unstake_unlocks(&mut projected_accounts, block.header.height);
        let mut tx_hashes = HashSet::new();
        let mut priority_fees = 0u64;
        let mut total_gas_used = 0u64;
        let mut coinbase: Option<&Transaction> = None;

        for (index, tx) in block.transactions.iter().enumerate() {
            if tx.chain_id != self.genesis_config.chain_id {
                return Err(ChainError::InvalidChainId {
                    expected: self.genesis_config.chain_id.clone(),
                    got: tx.chain_id.clone(),
                });
            }

            let tx_hash = tx.hash();
            if !tx_hashes.insert(tx_hash) {
                return Err(ChainError::DuplicateTransaction);
            }

            if tx.is_coinbase() {
                if index != 0 {
                    return Err(ChainError::InvalidCoinbase);
                }
                if coinbase.is_some() {
                    return Err(ChainError::MultipleCoinbase);
                }
                coinbase = Some(tx);
                continue;
            }

            let gas_used = Self::apply_user_transaction(
                &mut projected_accounts,
                &mut projected_contracts,
                &mut projected_receipts,
                &mut projected_token_registry,
                &mut projected_governance,
                tx,
                block.header.height,
                self.unstake_delay_blocks,
                self.epoch_length,
                self.minimum_stake,
                block.header.base_fee_per_gas,
            )?;
            priority_fees = priority_fees.saturating_add(Self::priority_fee_for_transaction(
                tx,
                gas_used,
                block.header.base_fee_per_gas,
            ));
            total_gas_used = total_gas_used.saturating_add(gas_used);
            if total_gas_used > self.block_gas_limit {
                return Err(ChainError::InvalidTransactionFormat(
                    "block gas limit exceeded",
                ));
            }
        }

        let total_active_stake =
            self.active_validator_total_stake(&projected_accounts, block.header.height);
        let _ = projected_governance.process_block(
            block.header.height,
            total_active_stake,
            self.epoch_length,
        );
        if block.header.gas_used != total_gas_used {
            return Err(ChainError::InvalidTransactionFormat(
                "block gas accounting mismatch",
            ));
        }

        let coinbase = coinbase.ok_or(ChainError::MissingCoinbase)?;
        if coinbase.to != proposer_address {
            return Err(ChainError::InvalidCoinbase);
        }
        if coinbase.amount != self.block_reward.saturating_add(priority_fees) {
            return Err(ChainError::InvalidCoinbase);
        }
        Self::apply_coinbase_transaction(&mut projected_accounts, coinbase)?;

        // Dispatch via protocol version derived from the block's
        // height so we validate v6 SMT roots once that hardfork is
        // active. At the v5 baseline this dispatcher returns the same
        // bytes as the prior `compute_state_root_full` call.
        let block_protocol_version = self.protocol_version_at_height(block.header.height);
        let computed_state_root = Self::compute_state_root_at_protocol(
            &projected_accounts,
            &projected_contracts,
            block_protocol_version,
        );
        if block.header.state_root != computed_state_root {
            // Diagnostic dump: when this fires on restart it crash-loops the
            // node, and historically we couldn't tell *what* part of the
            // recomputed state diverged. Log both roots plus a hash of every
            // account and contract leaf so the next occurrence can be
            // forensically reproduced. (#2)
            tracing::error!(
                target: "audit",
                event = "state_root_mismatch",
                height = block.header.height,
                expected = %hex::encode(&block.header.state_root),
                computed = %hex::encode(&computed_state_root),
                account_count = projected_accounts.len(),
                contract_count = projected_contracts.len(),
            );
            let mut sorted_accounts: Vec<(&Vec<u8>, &AccountState)> =
                projected_accounts.iter().collect();
            sorted_accounts.sort_by_key(|(a, _)| *a);
            for (addr, state) in sorted_accounts.iter().take(64) {
                let leaf = crate::core::state_root::account_leaf_hash(addr, state);
                tracing::error!(
                    target: "audit",
                    event = "state_root_account_leaf",
                    addr = %hex::encode(addr),
                    balance = state.balance,
                    nonce = state.nonce,
                    staked = state.staked_balance,
                    pending_unstakes = state.pending_unstakes.len(),
                    leaf = %hex::encode(&leaf),
                );
            }
            let mut sorted_contracts: Vec<(&Vec<u8>, &ContractState)> =
                projected_contracts.iter().collect();
            sorted_contracts.sort_by_key(|(a, _)| *a);
            for (addr, state) in sorted_contracts.iter().take(64) {
                let leaf = crate::core::state_root::contract_leaf_hash(addr, state);
                tracing::error!(
                    target: "audit",
                    event = "state_root_contract_leaf",
                    addr = %hex::encode(addr),
                    storage_keys = state.storage.len(),
                    leaf = %hex::encode(&leaf),
                );
            }
            return Err(ChainError::InvalidStateRoot);
        }

        Ok(BlockExecution {
            accounts: projected_accounts,
            contracts: projected_contracts,
            receipts: projected_receipts,
            token_registry: projected_token_registry,
            governance: projected_governance,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_user_transaction(
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        contracts: &mut HashMap<Vec<u8>, ContractState>,
        receipts: &mut HashMap<Vec<u8>, Receipt>,
        token_registry: &mut TokenRegistry,
        governance: &mut GovernanceState,
        tx: &Transaction,
        current_height: u64,
        unstake_delay_blocks: u64,
        epoch_length: u64,
        minimum_stake: u64,
        base_fee_per_gas: u64,
    ) -> Result<u64, ChainError> {
        Self::validate_transaction_shape(tx)?;

        if !tx.verify_signature() {
            return Err(ChainError::InvalidSignature);
        }

        // Native txs derive `from` from a Dilithium public key. EVM txs
        // recover `from` from the secp256k1 signature inside `verify_signature`
        // and skip this derivation (sender_public_key is empty).
        if !tx.is_evm() {
            let sender_address = hash::address_bytes_from_public_key(&tx.sender_public_key);
            if tx.from != sender_address {
                return Err(ChainError::InvalidSender);
            }
        }

        {
            let sender = accounts.entry(tx.from.clone()).or_default();
            if !tx.is_evm() {
                if let Some(existing_public_key) = &sender.public_key {
                    if existing_public_key != &tx.sender_public_key {
                        return Err(ChainError::InvalidSender);
                    }
                } else {
                    sender.public_key = Some(tx.sender_public_key.clone());
                }
            }

            let needed = tx.amount.saturating_add(tx.total_fee_cap());
            if sender.balance < needed {
                return Err(ChainError::InsufficientBalance {
                    address: hex::encode(&tx.from),
                    balance: sender.balance,
                    needed,
                });
            }

            if tx.nonce != sender.nonce {
                return Err(ChainError::InvalidNonce {
                    expected: sender.nonce,
                    got: tx.nonce,
                });
            }

            sender.balance -= needed;
            sender.nonce += 1;

            if tx.is_stake() {
                let was_below_minimum = sender.staked_balance < minimum_stake;
                sender.staked_balance = sender.staked_balance.saturating_add(tx.amount);
                if was_below_minimum && sender.staked_balance >= minimum_stake {
                    sender.validator_active_from_height =
                        Self::next_epoch_start_height_for(current_height, epoch_length);
                }
            }

            if tx.is_unstake() {
                if sender.staked_balance < tx.amount {
                    return Err(ChainError::InsufficientBalance {
                        address: hex::encode(&tx.from),
                        balance: sender.staked_balance,
                        needed: tx.amount,
                    });
                }
                sender.staked_balance = sender.staked_balance.saturating_sub(tx.amount);
                sender.pending_unstakes.push(super::PendingUnstake {
                    amount: tx.amount,
                    unlock_height: current_height.saturating_add(unstake_delay_blocks),
                });
            }
        }

        let tx_hash = tx.hash();
        let mut maybe_receipt: Option<Receipt> = None;
        let gas_used = match tx.kind {
            TransactionKind::Transfer => {
                let recipient = accounts.entry(tx.to.clone()).or_default();
                recipient.balance = recipient.balance.saturating_add(tx.amount);
                crate::vm::gas::GAS_BASE_TX
            }
            TransactionKind::Stake => crate::vm::gas::GAS_BASE_TX,
            TransactionKind::Unstake => crate::vm::gas::GAS_BASE_TX,
            TransactionKind::Coinbase => Err(ChainError::InvalidTransactionFormat(
                "coinbase not allowed in user transaction flow",
            ))?,
            TransactionKind::DeployContract => {
                let (contract, mut receipt) = Vm::deploy(
                    &tx.to,
                    &tx.from,
                    tx.nonce.saturating_sub(1),
                    tx.gas_limit,
                    current_height,
                )?;
                let deploy_gas_used = receipt.gas_used;
                receipt.tx_hash = tx_hash.clone();
                if let Some(ref addr) = receipt.contract_address {
                    contracts.insert(addr.clone(), contract);
                }
                maybe_receipt = Some(receipt);
                deploy_gas_used
            }
            TransactionKind::CallContract => {
                let contract = contracts
                    .get_mut(&tx.to)
                    .ok_or_else(|| ChainError::ContractNotFound(hex::encode(&tx.to)))?;
                let mut receipt = Vm::call(
                    contract,
                    &tx.to,
                    &tx.data,
                    &tx.from,
                    tx.amount,
                    tx.gas_limit,
                )?;
                let call_gas_used = receipt.gas_used;
                receipt.tx_hash = tx_hash.clone();
                // Credit the contract's implicit balance via the recipient account
                if tx.amount > 0 {
                    let recipient = accounts.entry(tx.to.clone()).or_default();
                    recipient.balance = recipient.balance.saturating_add(tx.amount);
                }
                maybe_receipt = Some(receipt);
                call_gas_used
            }
            TransactionKind::DeployToken
            | TransactionKind::TokenTransfer
            | TransactionKind::TokenApprove
            | TransactionKind::TokenTransferFrom
            | TransactionKind::SubmitProposal
            | TransactionKind::GovernanceVote => {
                Self::apply_token_or_governance_tx(
                    token_registry,
                    governance,
                    accounts,
                    minimum_stake,
                    epoch_length,
                    tx,
                    current_height,
                )?;
                crate::vm::gas::GAS_BASE_TX
            }
            TransactionKind::DeployEvmContract => {
                let outcome = Self::apply_evm_deploy(
                    accounts,
                    contracts,
                    tx,
                    current_height,
                    base_fee_per_gas,
                )?;
                let evm_gas_used = outcome.gas_used.max(crate::vm::gas::GAS_BASE_TX);
                let receipt = crate::vm::evm::receipt_from_outcome(
                    &outcome,
                    tx_hash.clone(),
                    base_fee_per_gas.max(1),
                );
                maybe_receipt = Some(receipt);
                evm_gas_used
            }
            TransactionKind::CallEvmContract => {
                let outcome = Self::apply_evm_call(
                    accounts,
                    contracts,
                    tx,
                    current_height,
                    base_fee_per_gas,
                )?;
                let evm_gas_used = outcome.gas_used.max(crate::vm::gas::GAS_BASE_TX);
                let receipt = crate::vm::evm::receipt_from_outcome(
                    &outcome,
                    tx_hash.clone(),
                    base_fee_per_gas.max(1),
                );
                maybe_receipt = Some(receipt);
                evm_gas_used
            }
        };

        Self::ensure_transaction_fee_covers_base(tx, gas_used, base_fee_per_gas)?;
        let effective_gas_price = tx
            .effective_gas_price(base_fee_per_gas)
            .ok_or(ChainError::FeeTooLow)?;
        let actual_fee_paid = gas_used.saturating_mul(effective_gas_price);
        let gas_refunded = tx.total_fee_cap().saturating_sub(actual_fee_paid);
        let priority_fee_paid = Self::priority_fee_for_transaction(tx, gas_used, base_fee_per_gas);
        let base_fee_burned = gas_used.saturating_mul(base_fee_per_gas);

        let sender = accounts.entry(tx.from.clone()).or_default();
        sender.balance = sender.balance.saturating_add(gas_refunded);

        if let Some(mut receipt) = maybe_receipt {
            receipt.effective_gas_price = effective_gas_price;
            receipt.priority_fee_paid = priority_fee_paid;
            receipt.base_fee_burned = base_fee_burned;
            receipt.gas_refunded = gas_refunded;
            receipts.insert(tx_hash, receipt);
        }

        Ok(gas_used)
    }

    pub(super) fn validate_transaction_shape(tx: &Transaction) -> Result<(), ChainError> {
        if !tx.is_coinbase() && tx.max_priority_fee_per_gas() > tx.max_fee_per_gas() {
            return Err(ChainError::InvalidTransactionFormat(
                "max_priority_fee_per_gas cannot exceed max_fee_per_gas",
            ));
        }

        match tx.kind {
            TransactionKind::Coinbase => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidCoinbase);
                }
                if tx.from != vec![0; hash::ADDRESS_LEN] {
                    return Err(ChainError::InvalidCoinbase);
                }
                if !tx.sender_public_key.is_empty() {
                    return Err(ChainError::InvalidCoinbase);
                }
                if tx.to.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidCoinbase);
                }
                if tx.fee != 0
                    || tx.max_fee_per_gas() != 0
                    || tx.max_priority_fee_per_gas() != 0
                    || tx.nonce != 0
                    || tx.signature.is_some()
                {
                    return Err(ChainError::InvalidCoinbase);
                }
                Ok(())
            }
            TransactionKind::Transfer => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN || tx.to.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidRecipient);
                }
                if tx.amount == 0 {
                    return Err(ChainError::InvalidTransactionFormat(
                        "transfer amount must be positive",
                    ));
                }
                Ok(())
            }
            TransactionKind::Stake | TransactionKind::Unstake => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if !tx.to.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "stake/unstake transactions cannot have a recipient",
                    ));
                }
                if tx.amount == 0 {
                    return Err(ChainError::InvalidTransactionFormat(
                        "stake/unstake amount must be positive",
                    ));
                }
                Ok(())
            }
            TransactionKind::DeployContract => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.to.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "deploy contract must include bytecode",
                    ));
                }
                if tx.gas_limit == 0 {
                    return Err(ChainError::InvalidTransactionFormat(
                        "deploy contract must specify gas_limit",
                    ));
                }
                Ok(())
            }
            TransactionKind::CallContract => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.to.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidTransactionFormat(
                        "call contract must specify a valid contract address",
                    ));
                }
                if tx.gas_limit == 0 {
                    return Err(ChainError::InvalidTransactionFormat(
                        "call contract must specify gas_limit",
                    ));
                }
                Ok(())
            }
            TransactionKind::DeployToken => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.data.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "deploy token must include token parameters in data",
                    ));
                }
                Ok(())
            }
            TransactionKind::TokenTransfer => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.data.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "token transfer must include transfer parameters in data",
                    ));
                }
                Ok(())
            }
            TransactionKind::TokenApprove => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.data.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "token approve must include approve parameters in data",
                    ));
                }
                Ok(())
            }
            TransactionKind::TokenTransferFrom => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.data.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "token transferFrom must include parameters in data",
                    ));
                }
                Ok(())
            }
            TransactionKind::SubmitProposal => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.data.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "submit proposal must include proposal parameters in data",
                    ));
                }
                Ok(())
            }
            TransactionKind::GovernanceVote => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.data.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "governance vote must include vote parameters in data",
                    ));
                }
                Ok(())
            }
            TransactionKind::DeployEvmContract => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.data.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "evm deploy must include init bytecode in data",
                    ));
                }
                if tx.gas_limit == 0 {
                    return Err(ChainError::InvalidTransactionFormat(
                        "evm deploy must specify gas_limit",
                    ));
                }
                if tx.evm_raw_tx.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "evm deploy missing raw tx for signature recovery",
                    ));
                }
                Ok(())
            }
            TransactionKind::CallEvmContract => {
                if tx.chain_id.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "missing transaction chain id",
                    ));
                }
                if tx.from.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidSender);
                }
                if tx.to.len() != hash::ADDRESS_LEN {
                    return Err(ChainError::InvalidTransactionFormat(
                        "evm call must specify a valid contract address",
                    ));
                }
                if tx.gas_limit == 0 {
                    return Err(ChainError::InvalidTransactionFormat(
                        "evm call must specify gas_limit",
                    ));
                }
                if tx.evm_raw_tx.is_empty() {
                    return Err(ChainError::InvalidTransactionFormat(
                        "evm call missing raw tx for signature recovery",
                    ));
                }
                Ok(())
            }
        }
    }

    pub(super) fn apply_coinbase_transaction(
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        tx: &Transaction,
    ) -> Result<(), ChainError> {
        Self::validate_transaction_shape(tx)?;
        if !tx.is_coinbase() {
            return Err(ChainError::InvalidCoinbase);
        }

        let recipient = accounts.entry(tx.to.clone()).or_default();
        recipient.balance = recipient.balance.saturating_add(tx.amount);
        Ok(())
    }

    pub(super) fn apply_unstake_unlocks(
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        block_height: u64,
    ) {
        for account in accounts.values_mut() {
            let mut released = 0u64;
            account.pending_unstakes.retain(|pending| {
                if pending.unlock_height <= block_height {
                    released = released.saturating_add(pending.amount);
                    false
                } else {
                    true
                }
            });
            account.balance = account.balance.saturating_add(released);
        }
    }

    pub(super) fn ensure_transaction_fee_covers_base(
        tx: &Transaction,
        gas_used: u64,
        base_fee_per_gas: u64,
    ) -> Result<(), ChainError> {
        let required = gas_used.saturating_mul(base_fee_per_gas);
        if tx.max_fee_per_gas() < base_fee_per_gas || tx.total_fee_cap() < required {
            return Err(ChainError::FeeTooLow);
        }
        Ok(())
    }

    pub(super) fn priority_fee_for_transaction(
        tx: &Transaction,
        gas_used: u64,
        base_fee_per_gas: u64,
    ) -> u64 {
        tx.priority_fee_per_gas(base_fee_per_gas)
            .unwrap_or_default()
            .saturating_mul(gas_used)
    }

    pub(super) fn apply_token_or_governance_tx(
        token_registry: &mut TokenRegistry,
        governance: &mut GovernanceState,
        accounts: &HashMap<Vec<u8>, AccountState>,
        minimum_stake: u64,
        epoch_length: u64,
        tx: &Transaction,
        current_height: u64,
    ) -> Result<(), ChainError> {
        match tx.kind {
            TransactionKind::DeployToken => {
                let params: crate::token::DeployTokenParams = serde_json::from_slice(&tx.data)
                    .map_err(|_| {
                        ChainError::InvalidTransactionFormat("invalid DeployToken JSON in data")
                    })?;
                token_registry
                    .deploy_token(
                        &tx.from,
                        tx.nonce.saturating_sub(1),
                        &params,
                        current_height,
                    )
                    .map_err(|_e| ChainError::InvalidTransactionFormat("token operation failed"))?;
            }
            TransactionKind::TokenTransfer => {
                let params: crate::token::TokenTransferParams = serde_json::from_slice(&tx.data)
                    .map_err(|_| {
                        ChainError::InvalidTransactionFormat("invalid TokenTransfer JSON in data")
                    })?;
                token_registry
                    .transfer(
                        &params.token_address,
                        &tx.from,
                        &params.recipient,
                        params.amount,
                    )
                    .map_err(|_e| ChainError::InvalidTransactionFormat("token operation failed"))?;
            }
            TransactionKind::TokenApprove => {
                let params: crate::token::TokenApproveParams = serde_json::from_slice(&tx.data)
                    .map_err(|_| {
                        ChainError::InvalidTransactionFormat("invalid TokenApprove JSON in data")
                    })?;
                token_registry
                    .approve(
                        &params.token_address,
                        &tx.from,
                        &params.spender,
                        params.amount,
                    )
                    .map_err(|_e| ChainError::InvalidTransactionFormat("token operation failed"))?;
            }
            TransactionKind::TokenTransferFrom => {
                let params: crate::token::TokenTransferFromParams =
                    serde_json::from_slice(&tx.data).map_err(|_| {
                        ChainError::InvalidTransactionFormat(
                            "invalid TokenTransferFrom JSON in data",
                        )
                    })?;
                token_registry
                    .transfer_from(
                        &params.token_address,
                        &tx.from,
                        &params.from,
                        &params.recipient,
                        params.amount,
                    )
                    .map_err(|_e| ChainError::InvalidTransactionFormat("token operation failed"))?;
            }
            TransactionKind::SubmitProposal => {
                let params: crate::governance::SubmitProposalParams =
                    serde_json::from_slice(&tx.data).map_err(|_| {
                        ChainError::InvalidTransactionFormat("invalid SubmitProposal JSON in data")
                    })?;
                // Verify sender is a validator
                let sender_account = accounts.get(&tx.from);
                let is_validator =
                    sender_account.is_some_and(|a| a.staked_balance >= minimum_stake);
                if !is_validator {
                    return Err(ChainError::UnauthorizedValidator);
                }
                // Snapshot all validator stakes at proposal creation time
                let stake_snapshot: std::collections::HashMap<Vec<u8>, u64> = accounts
                    .iter()
                    .filter(|(_, a)| a.staked_balance >= minimum_stake)
                    .map(|(addr, a)| (addr.clone(), a.staked_balance))
                    .collect();
                governance
                    .submit_proposal(
                        &tx.from,
                        &params,
                        current_height,
                        epoch_length,
                        stake_snapshot,
                    )
                    .map_err(|_| {
                        ChainError::InvalidTransactionFormat("governance operation failed")
                    })?;
            }
            TransactionKind::GovernanceVote => {
                let params: crate::governance::GovernanceVoteParams =
                    serde_json::from_slice(&tx.data).map_err(|_| {
                        ChainError::InvalidTransactionFormat("invalid GovernanceVote JSON in data")
                    })?;
                // Get voter stake
                let voter_stake = accounts
                    .get(&tx.from)
                    .map(|a| a.staked_balance)
                    .unwrap_or(0);
                if voter_stake < minimum_stake {
                    return Err(ChainError::UnauthorizedValidator);
                }
                governance
                    .vote(
                        &tx.from,
                        &params.proposal_id,
                        &params.vote,
                        voter_stake,
                        current_height,
                    )
                    .map_err(|_e| {
                        ChainError::InvalidTransactionFormat("governance operation failed")
                    })?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Apply an EVM-style `DeployEvmContract` tx by building a fresh state
    /// view from the chain accounts/contracts, running revm, then merging
    /// the delta back. Sender balance/nonce updates from revm are applied
    /// after the surrounding fee-handling block in `apply_user_transaction`
    /// has already debited the sender, so we re-apply revm's nonce
    /// faithfully (it's identical to ours except for the +1 we did).
    pub(super) fn apply_evm_deploy(
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        contracts: &mut HashMap<Vec<u8>, ContractState>,
        tx: &Transaction,
        current_height: u64,
        base_fee_per_gas: u64,
    ) -> Result<crate::vm::evm::EvmOutcome, ChainError> {
        // `apply_user_transaction` already incremented sender.nonce above.
        // For CREATE, revm computes the contract address from the
        // sender's nonce in the state view (= keccak(rlp([sender, nonce]))[12:]),
        // and that nonce is the *pre-tx* one (the one in the tx itself).
        // Without this fix, the state view we hand to revm has nonce = tx.nonce
        // + 1, so revm derives a contract address one nonce ahead of standard
        // Ethereum (Token at nonce 0 ends up where Faucet at nonce 1 should be).
        // Compensate by handing revm a state view where the caller's nonce is
        // tx.nonce (pre-increment).
        let mut state = Self::evm_state_view(accounts, contracts);
        let mut caller = [0u8; 20];
        if tx.from.len() == 20 {
            caller.copy_from_slice(&tx.from);
        }
        if let Some((bal, _post_nonce)) = state.accounts.get(&caller) {
            let pre_nonce = tx.nonce;
            state.accounts.insert(caller, (*bal, pre_nonce));
        }
        let outcome = crate::vm::evm::deploy(
            state,
            caller,
            &tx.data,
            tx.amount,
            tx.gas_limit,
            base_fee_per_gas.max(1),
            current_height,
            base_fee_per_gas,
            super::DEFAULT_BLOCK_GAS_LIMIT,
        )
        .map_err(|e| ChainError::InvalidTransactionFormat(Self::leak_evm_err(e)))?;
        Self::merge_evm_outcome(accounts, contracts, &outcome);
        Ok(outcome)
    }

    pub(super) fn apply_evm_call(
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        contracts: &mut HashMap<Vec<u8>, ContractState>,
        tx: &Transaction,
        current_height: u64,
        base_fee_per_gas: u64,
    ) -> Result<crate::vm::evm::EvmOutcome, ChainError> {
        let state = Self::evm_state_view(accounts, contracts);
        let mut caller = [0u8; 20];
        if tx.from.len() == 20 {
            caller.copy_from_slice(&tx.from);
        }
        let mut to = [0u8; 20];
        if tx.to.len() == 20 {
            to.copy_from_slice(&tx.to);
        }
        let outcome = crate::vm::evm::call(
            state,
            caller,
            to,
            &tx.data,
            tx.amount,
            tx.gas_limit,
            base_fee_per_gas.max(1),
            current_height,
            base_fee_per_gas,
            super::DEFAULT_BLOCK_GAS_LIMIT,
        )
        .map_err(|e| ChainError::InvalidTransactionFormat(Self::leak_evm_err(e)))?;
        Self::merge_evm_outcome(accounts, contracts, &outcome);
        Ok(outcome)
    }

    fn evm_state_view(
        accounts: &HashMap<Vec<u8>, AccountState>,
        contracts: &HashMap<Vec<u8>, ContractState>,
    ) -> crate::vm::evm::EvmStateView {
        let mut view = crate::vm::evm::EvmStateView::new();
        for (addr, account) in accounts {
            if addr.len() != 20 {
                continue;
            }
            let mut key = [0u8; 20];
            key.copy_from_slice(addr);
            view.insert_account(key, account.balance, account.nonce);
        }
        for (addr, contract) in contracts {
            if addr.len() != 20 {
                continue;
            }
            let mut key = [0u8; 20];
            key.copy_from_slice(addr);
            view.insert_contract(key, contract.clone());
        }
        view
    }

    fn merge_evm_outcome(
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        contracts: &mut HashMap<Vec<u8>, ContractState>,
        outcome: &crate::vm::evm::EvmOutcome,
    ) {
        for (addr, (balance, nonce)) in &outcome.account_updates {
            let entry = accounts.entry(addr.to_vec()).or_default();
            entry.balance = *balance;
            entry.nonce = *nonce;
        }
        for (addr, contract) in &outcome.contracts_created {
            // Merge: keep storage we computed below from storage_updates
            let merged_storage = outcome
                .storage_updates
                .get(addr)
                .cloned()
                .unwrap_or_else(|| contract.storage.clone());
            contracts.insert(
                addr.to_vec(),
                ContractState {
                    code_hash: contract.code_hash.clone(),
                    code: contract.code.clone(),
                    storage: merged_storage,
                    owner: contract.owner.clone(),
                },
            );
        }
        // Apply storage updates to existing contracts (call kind).
        for (addr, slot_updates) in &outcome.storage_updates {
            if outcome.contracts_created.contains_key(addr) {
                continue;
            }
            if let Some(contract) = contracts.get_mut(&addr.to_vec()) {
                for (slot, value) in slot_updates {
                    contract.storage.insert(slot.clone(), value.clone());
                }
            }
        }
    }

    fn leak_evm_err(err: crate::vm::evm::EvmError) -> &'static str {
        // Convert any EVM error to a small static label for the surrounding
        // `InvalidTransactionFormat(&'static str)`. Preserving exact reverts
        // is the receipt's job (success=false, return_data carries the
        // revert reason).
        match err {
            crate::vm::evm::EvmError::EmptyBytecode => "evm deploy: empty bytecode",
            crate::vm::evm::EvmError::InvalidBytecode => "evm: invalid bytecode",
            crate::vm::evm::EvmError::ContractNotFound => "evm: contract not found",
            crate::vm::evm::EvmError::OutOfGas => "evm: out of gas",
            crate::vm::evm::EvmError::Reverted(_) => "evm: reverted",
            crate::vm::evm::EvmError::Halted(_) => "evm: halted",
            crate::vm::evm::EvmError::Internal(_) => "evm: internal error",
            crate::vm::evm::EvmError::RlpDecode(_) => "evm: rlp decode failed",
            crate::vm::evm::EvmError::SignatureRecovery => "evm: signature recovery failed",
        }
    }
}
