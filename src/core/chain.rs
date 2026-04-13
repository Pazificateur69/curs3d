use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::consensus::{
    EpochSnapshot, EquivocationEvidence, FinalityTracker, FinalityVote, FinalizedBlock,
    ProofOfStake,
};
use crate::core::block::{Block, EMPTY_STATE_ROOT_SEED};
use crate::core::blocktree::{BlockTree, BlockTreeError};
use crate::core::receipt::Receipt;
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::dilithium::KeyPair;
use crate::crypto::hash;
use crate::storage::{Storage, StorageError};
use crate::vm::state::ContractState;
use crate::vm::{Vm, VmError};
use thiserror::Error;

pub const DEFAULT_BLOCK_GAS_LIMIT: u64 = 10_000_000;
pub const DEFAULT_BLOCK_REWARD: u64 = 50_000_000;
pub const DEFAULT_MIN_STAKE: u64 = 1_000_000_000;
pub const DEFAULT_UNSTAKE_DELAY_BLOCKS: u64 = 10;
pub const DEFAULT_EPOCH_LENGTH: u64 = 32;
pub const DEFAULT_JAIL_DURATION_BLOCKS: u64 = 64;
const MAX_FUTURE_BLOCK_TIME_SECS: i64 = 30;
const MAX_PENDING_TRANSACTIONS: usize = 10_000;
const MAX_PENDING_TRANSACTIONS_PER_ACCOUNT: usize = 64;
const CHAIN_CONFIG_KEY: &[u8] = b"chain_config";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenesisAllocation {
    pub public_key: String,
    pub balance: u64,
    pub staked_balance: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProtocolUpgrade {
    pub height: u64,
    pub version: u32,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenesisConfig {
    #[serde(default = "default_chain_id")]
    pub chain_id: String,
    #[serde(default = "default_chain_name")]
    pub chain_name: String,
    #[serde(default = "default_block_reward")]
    pub block_reward: u64,
    #[serde(default = "default_minimum_stake")]
    pub minimum_stake: u64,
    #[serde(default = "default_unstake_delay_blocks")]
    pub unstake_delay_blocks: u64,
    #[serde(default = "default_epoch_length")]
    pub epoch_length: u64,
    #[serde(default = "default_jail_duration_blocks")]
    pub jail_duration_blocks: u64,
    #[serde(default)]
    pub allocations: Vec<GenesisAllocation>,
    #[serde(default)]
    pub upgrades: Vec<ProtocolUpgrade>,
    #[serde(default = "default_block_gas_limit")]
    pub block_gas_limit: u64,
}

fn default_chain_id() -> String {
    "curs3d-devnet".to_string()
}

fn default_chain_name() -> String {
    "curs3d-devnet".to_string()
}

fn default_block_reward() -> u64 {
    DEFAULT_BLOCK_REWARD
}

fn default_minimum_stake() -> u64 {
    DEFAULT_MIN_STAKE
}

fn default_unstake_delay_blocks() -> u64 {
    DEFAULT_UNSTAKE_DELAY_BLOCKS
}

fn default_epoch_length() -> u64 {
    DEFAULT_EPOCH_LENGTH
}

fn default_jail_duration_blocks() -> u64 {
    DEFAULT_JAIL_DURATION_BLOCKS
}

fn default_block_gas_limit() -> u64 {
    DEFAULT_BLOCK_GAS_LIMIT
}

impl Default for GenesisConfig {
    fn default() -> Self {
        GenesisConfig {
            chain_id: "curs3d-devnet".to_string(),
            chain_name: "curs3d-devnet".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: DEFAULT_MIN_STAKE,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: Vec::new(),
            upgrades: Vec::new(),
            block_gas_limit: DEFAULT_BLOCK_GAS_LIMIT,
        }
    }
}

#[derive(Error, Debug)]
pub enum ChainError {
    #[error("invalid genesis config: {0}")]
    InvalidGenesis(String),
    #[error("genesis config does not match the chain already stored on disk")]
    GenesisMismatch,
    #[error("invalid block height: expected {expected}, got {got}")]
    InvalidHeight { expected: u64, got: u64 },
    #[error("invalid previous hash")]
    InvalidPrevHash,
    #[error("invalid block hash")]
    InvalidBlockHash,
    #[error("invalid merkle root")]
    InvalidMerkleRoot,
    #[error("invalid state root")]
    InvalidStateRoot,
    #[error("invalid block signature")]
    InvalidBlockSignature,
    #[error("invalid block timestamp: got {got}, expected between {min} and {max}")]
    InvalidBlockTimestamp { got: i64, min: i64, max: i64 },
    #[error("invalid transaction signature")]
    InvalidSignature,
    #[error("invalid chain id: expected {expected}, got {got}")]
    InvalidChainId { expected: String, got: String },
    #[error("invalid sender account")]
    InvalidSender,
    #[error("invalid recipient address")]
    InvalidRecipient,
    #[error("invalid transaction format: {0}")]
    InvalidTransactionFormat(&'static str),
    #[error("insufficient balance: {address} has {balance}, needs {needed}")]
    InsufficientBalance {
        address: String,
        balance: u64,
        needed: u64,
    },
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
    #[error("duplicate transaction")]
    DuplicateTransaction,
    #[error("mempool full")]
    MempoolFull,
    #[error("missing coinbase transaction")]
    MissingCoinbase,
    #[error("multiple coinbase transactions")]
    MultipleCoinbase,
    #[error("invalid coinbase transaction")]
    InvalidCoinbase,
    #[error("unauthorized validator")]
    UnauthorizedValidator,
    #[error("block tree error: {0}")]
    BlockTree(#[from] BlockTreeError),
    #[error("reorg blocked by finality at height {0}")]
    ReorgBelowFinality(u64),
    #[error("invalid protocol version: expected {expected}, got {got}")]
    InvalidProtocolVersion { expected: u32, got: u32 },
    #[error("snapshot error: {0}")]
    SnapshotError(String),
    #[error("vm error: {0}")]
    VmError(#[from] VmError),
    #[error("contract not found: {0}")]
    ContractNotFound(String),
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingUnstake {
    pub amount: u64,
    pub unlock_height: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountState {
    pub balance: u64,
    pub nonce: u64,
    pub staked_balance: u64,
    pub pending_unstakes: Vec<PendingUnstake>,
    pub validator_active_from_height: u64,
    pub jailed_until_height: u64,
    pub public_key: Option<Vec<u8>>,
}

pub struct Blockchain {
    pub blocks: Vec<Block>,
    pub accounts: HashMap<Vec<u8>, AccountState>,
    pub pending_transactions: Vec<Transaction>,
    pub block_reward: u64,
    pub minimum_stake: u64,
    pub unstake_delay_blocks: u64,
    pub epoch_length: u64,
    pub jail_duration_blocks: u64,
    pub genesis_config: GenesisConfig,
    pub block_tree: BlockTree,
    pub finality_tracker: FinalityTracker,
    pub slashed_validators: HashSet<Vec<u8>>,
    pub epoch_snapshots: HashMap<u64, EpochSnapshot>,
    pub block_gas_limit: u64,
    pub contracts: HashMap<Vec<u8>, ContractState>,
    pub receipts: HashMap<Vec<u8>, Receipt>,
    storage: Option<Storage>,
}

impl Blockchain {
    pub fn new() -> Self {
        Self::from_genesis(GenesisConfig::default()).expect("default genesis must be valid")
    }

    pub fn from_genesis(genesis_config: GenesisConfig) -> Result<Self, ChainError> {
        let accounts = Self::accounts_from_genesis(&genesis_config)?;
        let contracts = HashMap::new();
        let state_root = Self::compute_state_root_full(&accounts, &contracts);
        let genesis = Block::genesis_with_state_root(state_root, &genesis_config.chain_id);
        let block_tree = BlockTree::from_genesis(&genesis);

        Ok(Blockchain {
            blocks: vec![genesis],
            accounts,
            pending_transactions: Vec::new(),
            block_reward: genesis_config.block_reward,
            minimum_stake: genesis_config.minimum_stake,
            unstake_delay_blocks: genesis_config.unstake_delay_blocks,
            epoch_length: genesis_config.epoch_length,
            jail_duration_blocks: genesis_config.jail_duration_blocks,
            block_gas_limit: genesis_config.block_gas_limit,
            genesis_config,
            block_tree,
            finality_tracker: FinalityTracker::new(),
            slashed_validators: HashSet::new(),
            epoch_snapshots: HashMap::new(),
            contracts,
            receipts: HashMap::new(),
            storage: None,
        })
    }

    pub fn with_storage(
        data_dir: &str,
        genesis_config: Option<&GenesisConfig>,
    ) -> Result<Self, ChainError> {
        let storage = Storage::open(data_dir).map_err(StorageError::from)?;

        if let Some(stored_height) = storage.get_height()? {
            let schema_version = storage.get_schema_version()?.unwrap_or(1);
            let stored_genesis = storage
                .get_genesis_config_compat(CHAIN_CONFIG_KEY)?
                .unwrap_or_default();

            if let Some(expected_genesis) = genesis_config {
                if &stored_genesis != expected_genesis {
                    return Err(ChainError::GenesisMismatch);
                }
            }

            let expected_accounts = Self::accounts_from_genesis(&stored_genesis)?;
            let expected_genesis_hash = Block::genesis_with_state_root(
                Self::compute_state_root_full(&expected_accounts, &HashMap::new()),
                &stored_genesis.chain_id,
            )
            .hash;

            let mut blocks = Vec::new();
            for h in 0..=stored_height {
                if let Some(block) = storage.get_block_compat(h, &stored_genesis.chain_id)? {
                    blocks.push(block);
                } else {
                    break;
                }
            }

            if blocks.is_empty() || blocks[0].hash != expected_genesis_hash {
                return Err(ChainError::GenesisMismatch);
            }

            let mut accounts = HashMap::new();
            for (addr, state) in storage.get_all_accounts_compat()? {
                accounts.insert(addr, state);
            }

            let pending_transactions =
                storage.get_all_pending_transactions_compat(&stored_genesis.chain_id)?;
            let slashed_validators = storage.get_slashed_addresses()?;

            // Rebuild block tree from stored blocks
            let block_tree = if !blocks.is_empty() {
                let mut tree = BlockTree::from_genesis(&blocks[0]);
                for block in blocks.iter().skip(1) {
                    let proposer_stake = accounts
                        .get(&hash::address_bytes_from_public_key(
                            &block.header.validator_public_key,
                        ))
                        .map(|a| a.staked_balance)
                        .unwrap_or(0);
                    let _ = tree.insert(block.clone(), proposer_stake);
                }
                tree
            } else {
                BlockTree::from_genesis(&Block::genesis())
            };

            // Load finalized height from meta
            let finalized_height: u64 = storage
                .get_meta(b"finalized_height")?
                .unwrap_or(0);
            let finality_tracker = FinalityTracker::with_finalized(
                finalized_height,
                blocks
                    .get(finalized_height as usize)
                    .map(|b| b.hash.clone())
                    .unwrap_or_default(),
            );

            tracing::info!(
                "Loaded blockchain from disk: chain={}, height={}, accounts={}, pending={}, slashed={}, finalized={}",
                stored_genesis.chain_name,
                stored_height,
                accounts.len(),
                pending_transactions.len(),
                slashed_validators.len(),
                finalized_height,
            );

            // Load epoch snapshots from storage
            let mut epoch_snapshots = HashMap::new();
            if let Ok(snapshots) = storage.get_all_epoch_snapshots() {
                for (epoch, snapshot) in snapshots {
                    epoch_snapshots.insert(epoch, snapshot);
                }
            }

            let chain = Blockchain {
                blocks,
                accounts,
                pending_transactions,
                block_reward: stored_genesis.block_reward,
                minimum_stake: stored_genesis.minimum_stake,
                unstake_delay_blocks: stored_genesis.unstake_delay_blocks,
                epoch_length: stored_genesis.epoch_length,
                jail_duration_blocks: stored_genesis.jail_duration_blocks,
                block_gas_limit: stored_genesis.block_gas_limit,
                genesis_config: stored_genesis,
                block_tree,
                finality_tracker,
                slashed_validators,
                epoch_snapshots,
                contracts: HashMap::new(),
                receipts: HashMap::new(),
                storage: Some(storage),
            };

            if schema_version < crate::storage::CURRENT_SCHEMA_VERSION {
                if let Some(ref storage) = chain.storage {
                    storage.put_meta(CHAIN_CONFIG_KEY, &chain.genesis_config)?;
                    storage.put_meta(
                        crate::storage::SCHEMA_VERSION_KEY,
                        &crate::storage::CURRENT_SCHEMA_VERSION,
                    )?;
                    for block in &chain.blocks {
                        storage.put_block(block)?;
                    }
                    for (address, state) in &chain.accounts {
                        storage.put_account(address, state)?;
                    }
                    storage.replace_pending_transactions(&chain.pending_transactions)?;
                    storage.flush()?;
                }
            }

            Ok(chain)
        } else {
            let mut chain = Self::from_genesis(genesis_config.cloned().unwrap_or_default())?;
            chain.storage = Some(storage);

            if let Some(ref storage) = chain.storage {
                storage.put_meta(CHAIN_CONFIG_KEY, &chain.genesis_config)?;
                storage.put_block(chain.latest_block())?;
                for (address, state) in &chain.accounts {
                    storage.put_account(address, state)?;
                }
                storage.replace_pending_transactions(&[])?;
                storage.put_meta(b"finalized_height", &0u64)?;
                storage.put_meta(
                    crate::storage::SCHEMA_VERSION_KEY,
                    &crate::storage::CURRENT_SCHEMA_VERSION,
                )?;
                storage.flush()?;
            }

            tracing::info!(
                "Initialized new blockchain from genesis config: {}",
                chain.genesis_config.chain_name
            );

            Ok(chain)
        }
    }

    pub fn height(&self) -> u64 {
        self.blocks.len() as u64 - 1
    }

    pub fn latest_block(&self) -> &Block {
        self.blocks
            .last()
            .expect("chain must have at least genesis")
    }

    pub fn latest_hash(&self) -> &[u8] {
        &self.latest_block().hash
    }

    pub fn genesis_hash(&self) -> &[u8] {
        &self.blocks[0].hash
    }

    pub fn chain_id(&self) -> &str {
        &self.genesis_config.chain_id
    }

    pub fn current_epoch(&self) -> u64 {
        self.height() / self.epoch_length.max(1)
    }

    pub fn current_epoch_start_height(&self) -> u64 {
        self.current_epoch() * self.epoch_length.max(1)
    }

    pub fn epoch_for_height(&self, height: u64) -> u64 {
        height / self.epoch_length.max(1)
    }

    /// Compute and store an EpochSnapshot for the given epoch using the current accounts.
    pub fn create_epoch_snapshot(&mut self, epoch: u64) {
        let start_height = epoch * self.epoch_length.max(1);
        let pos = ProofOfStake::with_slashed(
            self.minimum_stake,
            self.slashed_validators.clone(),
            start_height,
        );
        let validators = pos.active_validators(&self.accounts);
        let total_stake: u64 = validators.iter().map(|v| v.stake).sum();
        let snapshot = EpochSnapshot {
            epoch,
            start_height,
            validators,
            total_stake,
        };

        // Persist to storage
        if let Some(ref storage) = self.storage {
            let _ = storage.put_epoch_snapshot(epoch, &snapshot);
        }

        self.epoch_snapshots.insert(epoch, snapshot);
    }

    /// Get the EpochSnapshot for a given epoch, if it exists.
    pub fn get_epoch_snapshot(&self, epoch: u64) -> Option<&EpochSnapshot> {
        self.epoch_snapshots.get(&epoch)
    }

    /// Return the protocol version that should be active at the given height.
    pub fn protocol_version_at_height(&self, height: u64) -> u32 {
        let mut version = 1u32;
        for upgrade in &self.genesis_config.upgrades {
            if upgrade.height <= height {
                version = upgrade.version;
            }
        }
        version
    }

    /// Create a state sync snapshot from the current chain state.
    pub fn create_snapshot(&self) -> Result<crate::storage::SnapshotManifest, ChainError> {
        let mut all_accounts: Vec<(&Vec<u8>, &AccountState)> = self.accounts.iter().collect();
        all_accounts.sort_by(|(a, _), (b, _)| a.cmp(b));

        let chunk_size = 1000;
        let mut chunks = Vec::new();
        let mut chunk_hashes = Vec::new();

        for chunk_accounts in all_accounts.chunks(chunk_size) {
            let data = bincode::serialize(&chunk_accounts)
                .map_err(|e| ChainError::SnapshotError(e.to_string()))?;
            let chunk_hash = hash::sha3_hash(&data);
            chunk_hashes.push(chunk_hash.clone());
            chunks.push(crate::storage::StateChunk {
                index: chunks.len(),
                data,
                hash: chunk_hash,
            });
        }

        let height = self.height();
        let epoch = self.current_epoch();
        let state_root = Self::compute_state_root(&self.accounts);

        // Persist chunks to storage
        if let Some(ref storage) = self.storage {
            for chunk in &chunks {
                let _ = storage.put_snapshot_chunk(height, chunk);
            }
        }

        let manifest = crate::storage::SnapshotManifest {
            height,
            epoch,
            state_root,
            chunk_count: chunks.len(),
            chunk_hashes,
        };

        // Persist manifest
        if let Some(ref storage) = self.storage {
            let _ = storage.put_snapshot_manifest(height, &manifest);
        }

        Ok(manifest)
    }

    /// Load a blockchain from a state sync snapshot.
    pub fn load_from_snapshot(
        manifest: &crate::storage::SnapshotManifest,
        chunks: &[crate::storage::StateChunk],
        genesis_config: GenesisConfig,
    ) -> Result<Self, ChainError> {
        // Verify chunk hashes
        if chunks.len() != manifest.chunk_count {
            return Err(ChainError::SnapshotError(format!(
                "expected {} chunks, got {}",
                manifest.chunk_count,
                chunks.len()
            )));
        }
        for (i, chunk) in chunks.iter().enumerate() {
            let computed_hash = hash::sha3_hash(&chunk.data);
            if i >= manifest.chunk_hashes.len() || computed_hash != manifest.chunk_hashes[i] {
                return Err(ChainError::SnapshotError(format!(
                    "chunk {} hash mismatch",
                    i
                )));
            }
        }

        // Reconstruct accounts from chunks
        let mut accounts = HashMap::new();
        for chunk in chunks {
            let chunk_accounts: Vec<(Vec<u8>, AccountState)> =
                bincode::deserialize(&chunk.data)
                    .map_err(|e| ChainError::SnapshotError(e.to_string()))?;
            for (addr, state) in chunk_accounts {
                accounts.insert(addr, state);
            }
        }

        // Verify state root
        let computed_root = Self::compute_state_root(&accounts);
        if computed_root != manifest.state_root {
            return Err(ChainError::SnapshotError(
                "state root mismatch".to_string(),
            ));
        }

        // Build a minimal chain from genesis
        let genesis_accounts = Self::accounts_from_genesis(&genesis_config)?;
        let state_root = Self::compute_state_root(&genesis_accounts);
        let genesis = Block::genesis_with_state_root(state_root, &genesis_config.chain_id);
        let block_tree = BlockTree::from_genesis(&genesis);

        Ok(Blockchain {
            blocks: vec![genesis],
            accounts,
            pending_transactions: Vec::new(),
            block_reward: genesis_config.block_reward,
            minimum_stake: genesis_config.minimum_stake,
            unstake_delay_blocks: genesis_config.unstake_delay_blocks,
            epoch_length: genesis_config.epoch_length,
            jail_duration_blocks: genesis_config.jail_duration_blocks,
            block_gas_limit: genesis_config.block_gas_limit,
            genesis_config,
            block_tree,
            finality_tracker: FinalityTracker::new(),
            slashed_validators: HashSet::new(),
            epoch_snapshots: HashMap::new(),
            contracts: HashMap::new(),
            receipts: HashMap::new(),
            storage: None,
        })
    }

    pub fn get_balance(&self, address: &[u8]) -> u64 {
        self.accounts.get(address).map(|a| a.balance).unwrap_or(0)
    }

    pub fn get_staked_balance(&self, address: &[u8]) -> u64 {
        self.accounts
            .get(address)
            .map(|a| a.staked_balance)
            .unwrap_or(0)
    }

    pub fn get_account(&self, address: &[u8]) -> AccountState {
        self.accounts.get(address).cloned().unwrap_or_default()
    }

    pub fn add_transaction(&mut self, tx: Transaction) -> Result<(), ChainError> {
        if tx.is_coinbase() {
            return Err(ChainError::InvalidTransactionFormat(
                "coinbase transactions cannot enter the mempool",
            ));
        }

        if tx.chain_id != self.genesis_config.chain_id {
            return Err(ChainError::InvalidChainId {
                expected: self.genesis_config.chain_id.clone(),
                got: tx.chain_id.clone(),
            });
        }

        if self.pending_transactions.len() >= MAX_PENDING_TRANSACTIONS {
            return Err(ChainError::MempoolFull);
        }

        let sender_pending = self
            .pending_transactions
            .iter()
            .filter(|pending| pending.from == tx.from)
            .count();
        if sender_pending >= MAX_PENDING_TRANSACTIONS_PER_ACCOUNT {
            return Err(ChainError::MempoolFull);
        }

        let tx_hash = tx.hash();
        if self
            .pending_transactions
            .iter()
            .any(|pending| pending.hash() == tx_hash)
        {
            return Err(ChainError::DuplicateTransaction);
        }

        let mut projected_accounts = self.accounts.clone();
        let mut projected_contracts = self.contracts.clone();
        let mut projected_receipts = HashMap::new();
        let mut seen_hashes = HashSet::new();
        for pending in &self.pending_transactions {
            let pending_hash = pending.hash();
            if !seen_hashes.insert(pending_hash) {
                return Err(ChainError::DuplicateTransaction);
            }
            Self::apply_user_transaction(
                &mut projected_accounts,
                &mut projected_contracts,
                &mut projected_receipts,
                pending,
                self.height() + 1,
                self.unstake_delay_blocks,
                self.epoch_length,
                self.minimum_stake,
            )?;
        }

        Self::apply_user_transaction(
            &mut projected_accounts,
            &mut projected_contracts,
            &mut projected_receipts,
            &tx,
            self.height() + 1,
            self.unstake_delay_blocks,
            self.epoch_length,
            self.minimum_stake,
        )?;
        self.pending_transactions.push(tx);
        self.pending_transactions
            .sort_by(|a, b| b.fee.cmp(&a.fee).then_with(|| a.timestamp.cmp(&b.timestamp)).then_with(|| a.nonce.cmp(&b.nonce)));
        self.persist_pending_transactions()?;
        Ok(())
    }

    pub fn create_block(&self, validator_keypair: &KeyPair) -> Result<Block, ChainError> {
        let prev_block = self.latest_block();
        let height = prev_block.header.height + 1;
        let prev_hash = prev_block.hash.clone();

        let proposer_public_key = validator_keypair.public_key.clone();
        let proposer_address = hash::address_bytes_from_public_key(&proposer_public_key);
        self.ensure_validator_is_authorized(&proposer_public_key, height, &prev_hash)?;

        let mut projected_accounts = self.accounts.clone();
        let mut projected_contracts = self.contracts.clone();
        let mut projected_receipts = HashMap::new();
        Self::apply_unstake_unlocks(&mut projected_accounts, height);
        let mut block_txs = Vec::new();
        let mut total_fees = 0u64;
        let mut seen_hashes = HashSet::new();

        for pending in &self.pending_transactions {
            let tx_hash = pending.hash();
            if !seen_hashes.insert(tx_hash) {
                continue;
            }

            if Self::apply_user_transaction(
                &mut projected_accounts,
                &mut projected_contracts,
                &mut projected_receipts,
                pending,
                height,
                self.unstake_delay_blocks,
                self.epoch_length,
                self.minimum_stake,
            )
            .is_ok()
            {
                total_fees = total_fees.saturating_add(pending.fee);
                block_txs.push(pending.clone());
            }
        }

        let coinbase = Transaction::coinbase(
            &self.genesis_config.chain_id,
            proposer_address.clone(),
            self.block_reward.saturating_add(total_fees),
        );
        Self::apply_coinbase_transaction(&mut projected_accounts, &coinbase)?;

        let mut transactions = vec![coinbase];
        transactions.extend(block_txs);

        Ok(Block::new(
            height,
            prev_hash,
            Self::compute_state_root_full(&projected_accounts, &projected_contracts),
            transactions,
            validator_keypair,
        ))
    }

    pub fn add_block(&mut self, block: Block) -> Result<(), ChainError> {
        let prev = self.latest_block();
        let projected_accounts = self.validate_block_against_state(&block, prev, &self.accounts)?;

        // Apply contract state changes from block transactions
        for tx in &block.transactions {
            match tx.kind {
                TransactionKind::DeployContract => {
                    if let Ok((contract, mut receipt)) =
                        Vm::deploy(&tx.to, &tx.from, tx.nonce.wrapping_sub(1), tx.gas_limit)
                    {
                        let tx_hash = tx.hash();
                        receipt.tx_hash = tx_hash.clone();
                        if let Some(ref addr) = receipt.contract_address {
                            self.contracts.insert(addr.clone(), contract);
                        }
                        self.receipts.insert(tx_hash, receipt);
                    }
                }
                TransactionKind::CallContract => {
                    if let Some(contract) = self.contracts.get_mut(&tx.to) {
                        if let Ok(mut receipt) =
                            Vm::call(contract, &tx.data, &tx.from, tx.amount, tx.gas_limit)
                        {
                            let tx_hash = tx.hash();
                            receipt.tx_hash = tx_hash.clone();
                            self.receipts.insert(tx_hash, receipt);
                        }
                    }
                }
                _ => {}
            }
        }

        // Insert into block tree for fork tracking
        let proposer_address =
            hash::address_bytes_from_public_key(&block.header.validator_public_key);
        let proposer_stake = projected_accounts
            .get(&proposer_address)
            .map(|a| a.staked_balance)
            .unwrap_or(0);
        // Ignore block tree errors for blocks already in the tree
        let _ = self.block_tree.insert(block.clone(), proposer_stake);

        let block_height = block.header.height;
        self.accounts = projected_accounts;
        self.blocks.push(block.clone());
        self.remove_block_transactions_from_mempool(&block);
        self.persist_block_state(&block)?;

        // Create epoch snapshot at epoch boundaries (height > 0 and height % epoch_length == 0)
        let epoch_len = self.epoch_length.max(1);
        if block_height > 0 && block_height % epoch_len == 0 {
            let epoch = block_height / epoch_len;
            if !self.epoch_snapshots.contains_key(&epoch) {
                self.create_epoch_snapshot(epoch);
            }
        }

        Ok(())
    }

    /// Add a finality vote. Returns Some(FinalizedBlock) if threshold reached.
    pub fn add_finality_vote(&mut self, vote: FinalityVote) -> Option<FinalizedBlock> {
        let voted_block = self.block_tree.get(&vote.block_hash)?;
        if voted_block.header.height != vote.block_height {
            return None;
        }
        if !self.block_tree.is_on_canonical_chain(&vote.block_hash) {
            return None;
        }

        let result = self.finality_tracker.add_vote(
            vote,
            &self.accounts,
            self.minimum_stake,
            &self.slashed_validators,
            self.height(),
        );

        if let Some(ref finalized) = result {
            self.block_tree
                .set_finalized(finalized.hash.clone(), finalized.height);

            // Persist finalized height
            if let Some(ref storage) = self.storage {
                let _ = storage.put_meta(b"finalized_height", &finalized.height);
                let _ = storage.flush();
            }

            tracing::info!(
                "Block #{} finalized (hash: {})",
                finalized.height,
                hex::encode(&finalized.hash[..8])
            );
        }

        result
    }

    /// Process equivocation evidence
    pub fn process_equivocation(
        &mut self,
        evidence: &EquivocationEvidence,
    ) -> Result<u64, crate::consensus::SlashingError> {
        let mut pos = ProofOfStake::with_slashed(
            self.minimum_stake,
            self.slashed_validators.clone(),
            self.height(),
        );
        let penalty = pos.slash_with_evidence(
            &mut self.accounts,
            evidence,
            self.jail_duration_blocks,
        )?;
        self.slashed_validators = pos.slashed_validators;

        // Persist
        if let Some(ref storage) = self.storage {
            let _ = storage.put_evidence(evidence);
            let address =
                hash::address_bytes_from_public_key(&evidence.validator_public_key);
            if let Some(state) = self.accounts.get(&address) {
                let _ = storage.put_account(&address, state);
            }
            let _ = storage.flush();
        }

        Ok(penalty)
    }

    pub fn finalized_height(&self) -> u64 {
        self.finality_tracker.finalized_height
    }

    pub fn active_validator_count(&self) -> usize {
        ProofOfStake::with_slashed(
            self.minimum_stake,
            self.slashed_validators.clone(),
            self.height() + 1,
        )
            .active_validators(&self.accounts)
            .len()
    }

    pub fn is_valid(&self) -> bool {
        let mut replay = match Self::from_genesis(self.genesis_config.clone()) {
            Ok(chain) => chain,
            Err(_) => return false,
        };

        for block in self.blocks.iter().skip(1) {
            if replay.add_block(block.clone()).is_err() {
                return false;
            }
        }

        replay.accounts == self.accounts
    }

    /// Attempt to add a block that may fork from the current canonical chain.
    /// If it builds on the tip, behaves like add_block.
    /// If it forks, inserts into the block tree and potentially reorgs.
    pub fn add_block_with_fork_choice(&mut self, block: Block) -> Result<bool, ChainError> {
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
        let parent_accounts = self.replay_accounts_to_tip(&parent.hash)?;
        let projected_accounts =
            self.validate_block_against_state(&block, &parent, &parent_accounts)?;

        // Get proposer stake for weight calculation
        let proposer_address =
            hash::address_bytes_from_public_key(&block.header.validator_public_key);
        let proposer_stake = projected_accounts
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
    fn reorg_to_canonical_tip(&mut self) -> Result<(), ChainError> {
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
                .unwrap_or_else(|| self.blocks[0].hash.clone());
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

        // Rebuild from genesis
        let mut accounts = Self::accounts_from_genesis(&self.genesis_config)?;
        let mut new_blocks = vec![canonical[0].clone()]; // genesis

        for block in canonical.iter().skip(1) {
            let parent = new_blocks.last().expect("canonical chain includes genesis");
            accounts = self.validate_block_against_state(block, parent, &accounts)?;

            new_blocks.push((*block).clone());
        }

        self.accounts = accounts;
        self.blocks = new_blocks;
        self.persist_full_state()?;

        tracing::info!(
            "Reorg complete. New height: {}, new tip: {}",
            self.height(),
            self.latest_block().hash_hex()
        );

        Ok(())
    }

    fn persist_full_state(&self) -> Result<(), ChainError> {
        if let Some(ref storage) = self.storage {
            for block in &self.blocks {
                storage.put_block(block)?;
            }
            for (address, state) in &self.accounts {
                storage.put_account(address, state)?;
            }
            storage.replace_pending_transactions(&self.pending_transactions)?;
            storage.flush()?;
        }
        Ok(())
    }

    pub fn compute_state_root(accounts: &HashMap<Vec<u8>, AccountState>) -> Vec<u8> {
        Self::compute_state_root_full(accounts, &HashMap::new())
    }

    pub fn compute_state_root_full(
        accounts: &HashMap<Vec<u8>, AccountState>,
        contracts: &HashMap<Vec<u8>, ContractState>,
    ) -> Vec<u8> {
        if accounts.is_empty() && contracts.is_empty() {
            return hash::sha3_hash(EMPTY_STATE_ROOT_SEED);
        }

        let mut leaves: Vec<Vec<u8>> = Vec::new();

        // Account leaves
        let mut account_entries: Vec<(&Vec<u8>, &AccountState)> = accounts.iter().collect();
        account_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (address, state) in account_entries {
            let encoded =
                bincode::serialize(&(address, state)).expect("failed to serialize state leaf");
            leaves.push(hash::sha3_hash(&encoded));
        }

        // Contract leaves
        let mut contract_entries: Vec<(&Vec<u8>, &ContractState)> = contracts.iter().collect();
        contract_entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (address, state) in contract_entries {
            let encoded = bincode::serialize(&(address, state))
                .expect("failed to serialize contract leaf");
            leaves.push(hash::sha3_hash(&encoded));
        }

        hash::merkle_root(&leaves)
    }

    fn accounts_from_genesis(
        genesis_config: &GenesisConfig,
    ) -> Result<HashMap<Vec<u8>, AccountState>, ChainError> {
        let mut accounts = HashMap::new();

        for allocation in &genesis_config.allocations {
            let public_key = Self::decode_public_key_hex(&allocation.public_key)?;
            let address = hash::address_bytes_from_public_key(&public_key);
            if accounts.contains_key(&address) {
                return Err(ChainError::InvalidGenesis(
                    "duplicate account in genesis".to_string(),
                ));
            }

            accounts.insert(
                address,
                AccountState {
                    balance: allocation.balance,
                    nonce: 0,
                    staked_balance: allocation.staked_balance,
                    pending_unstakes: Vec::new(),
                    validator_active_from_height: if allocation.staked_balance
                        >= genesis_config.minimum_stake
                    {
                        1
                    } else {
                        0
                    },
                    jailed_until_height: 0,
                    public_key: Some(public_key),
                },
            );
        }

        Ok(accounts)
    }

    fn decode_public_key_hex(value: &str) -> Result<Vec<u8>, ChainError> {
        let raw = value.strip_prefix("0x").unwrap_or(value);
        hex::decode(raw).map_err(|_| {
            ChainError::InvalidGenesis("invalid public_key hex in genesis".to_string())
        })
    }

    fn next_epoch_start_height_for(current_height: u64, epoch_length: u64) -> u64 {
        let epoch_length = epoch_length.max(1);
        current_height
            .saturating_div(epoch_length)
            .saturating_add(1)
            .saturating_mul(epoch_length)
    }

    fn ensure_validator_is_authorized(
        &self,
        validator_public_key: &[u8],
        block_height: u64,
        prev_hash: &[u8],
    ) -> Result<(), ChainError> {
        self.ensure_validator_is_authorized_for_accounts(
            &self.accounts,
            validator_public_key,
            block_height,
            prev_hash,
        )
    }

    fn ensure_validator_is_authorized_for_accounts(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
        validator_public_key: &[u8],
        block_height: u64,
        prev_hash: &[u8],
    ) -> Result<(), ChainError> {
        // Try to use frozen epoch snapshot for validator selection
        let epoch = block_height / self.epoch_length.max(1);
        if let Some(snapshot) = self.epoch_snapshots.get(&epoch) {
            match ProofOfStake::select_validator_from_snapshot(snapshot, block_height, prev_hash) {
                Some(expected) if expected.public_key == validator_public_key => return Ok(()),
                Some(_) => return Err(ChainError::UnauthorizedValidator),
                None => return Ok(()),
            }
        }

        // Fall back to live computation (genesis epoch or no snapshot yet)
        let pos =
            ProofOfStake::with_slashed(
                self.minimum_stake,
                self.slashed_validators.clone(),
                block_height,
            );
        match pos.select_validator(accounts, block_height, prev_hash) {
            Some(expected) if expected.public_key == validator_public_key => Ok(()),
            Some(_) => Err(ChainError::UnauthorizedValidator),
            None => Ok(()),
        }
    }

    fn validate_block_against_state(
        &self,
        block: &Block,
        parent: &Block,
        parent_accounts: &HashMap<Vec<u8>, AccountState>,
    ) -> Result<HashMap<Vec<u8>, AccountState>, ChainError> {
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

        self.ensure_validator_is_authorized_for_accounts(
            parent_accounts,
            &block.header.validator_public_key,
            block.header.height,
            &block.header.prev_hash,
        )?;

        let proposer_address =
            hash::address_bytes_from_public_key(&block.header.validator_public_key);
        let mut projected_accounts = parent_accounts.clone();
        let mut projected_contracts = self.contracts.clone();
        let mut projected_receipts = HashMap::new();
        Self::apply_unstake_unlocks(&mut projected_accounts, block.header.height);
        let mut tx_hashes = HashSet::new();
        let mut user_fees = 0u64;
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

            user_fees = user_fees.saturating_add(tx.fee);
            Self::apply_user_transaction(
                &mut projected_accounts,
                &mut projected_contracts,
                &mut projected_receipts,
                tx,
                block.header.height,
                self.unstake_delay_blocks,
                self.epoch_length,
                self.minimum_stake,
            )?;
        }

        let coinbase = coinbase.ok_or(ChainError::MissingCoinbase)?;
        if coinbase.to != proposer_address {
            return Err(ChainError::InvalidCoinbase);
        }
        if coinbase.amount != self.block_reward.saturating_add(user_fees) {
            return Err(ChainError::InvalidCoinbase);
        }
        Self::apply_coinbase_transaction(&mut projected_accounts, coinbase)?;

        if block.header.state_root
            != Self::compute_state_root_full(&projected_accounts, &projected_contracts)
        {
            return Err(ChainError::InvalidStateRoot);
        }

        Ok(projected_accounts)
    }

    fn replay_accounts_to_tip(
        &self,
        tip_hash: &[u8],
    ) -> Result<HashMap<Vec<u8>, AccountState>, ChainError> {
        let mut lineage = Vec::new();
        let mut current = tip_hash.to_vec();

        loop {
            let block = self
                .block_tree
                .get(&current)
                .ok_or(BlockTreeError::OrphanBlock)?
                .clone();
            lineage.push(block);
            if current == self.blocks[0].hash {
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
        let mut previous = lineage
            .first()
            .cloned()
            .expect("lineage always includes genesis");
        for block in lineage.iter().skip(1) {
            accounts = self.validate_block_against_state(block, &previous, &accounts)?;
            previous = block.clone();
        }

        Ok(accounts)
    }

    fn apply_user_transaction(
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        contracts: &mut HashMap<Vec<u8>, ContractState>,
        receipts: &mut HashMap<Vec<u8>, Receipt>,
        tx: &Transaction,
        current_height: u64,
        unstake_delay_blocks: u64,
        epoch_length: u64,
        minimum_stake: u64,
    ) -> Result<(), ChainError> {
        Self::validate_transaction_shape(tx)?;

        if !tx.verify_signature() {
            return Err(ChainError::InvalidSignature);
        }

        let sender_address = hash::address_bytes_from_public_key(&tx.sender_public_key);
        if tx.from != sender_address {
            return Err(ChainError::InvalidSender);
        }

        {
            let sender = accounts.entry(tx.from.clone()).or_default();
            if let Some(existing_public_key) = &sender.public_key {
                if existing_public_key != &tx.sender_public_key {
                    return Err(ChainError::InvalidSender);
                }
            } else {
                sender.public_key = Some(tx.sender_public_key.clone());
            }

            let needed = tx.amount.saturating_add(tx.fee);
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
                sender.pending_unstakes.push(PendingUnstake {
                    amount: tx.amount,
                    unlock_height: current_height.saturating_add(unstake_delay_blocks),
                });
            }
        }

        let tx_hash = tx.hash();

        match tx.kind {
            TransactionKind::Transfer => {
                let recipient = accounts.entry(tx.to.clone()).or_default();
                recipient.balance = recipient.balance.saturating_add(tx.amount);
            }
            TransactionKind::Stake => {}
            TransactionKind::Unstake => {}
            TransactionKind::Coinbase => {
                return Err(ChainError::InvalidTransactionFormat(
                    "coinbase not allowed in user transaction flow",
                ));
            }
            TransactionKind::DeployContract => {
                let (contract, mut receipt) =
                    Vm::deploy(&tx.to, &tx.from, tx.nonce.wrapping_sub(1), tx.gas_limit)?;
                receipt.tx_hash = tx_hash.clone();
                if let Some(ref addr) = receipt.contract_address {
                    contracts.insert(addr.clone(), contract);
                }
                receipts.insert(tx_hash, receipt);
            }
            TransactionKind::CallContract => {
                let contract = contracts.get_mut(&tx.to).ok_or_else(|| {
                    ChainError::ContractNotFound(hex::encode(&tx.to))
                })?;
                let mut receipt =
                    Vm::call(contract, &tx.data, &tx.from, tx.amount, tx.gas_limit)?;
                receipt.tx_hash = tx_hash.clone();
                // Credit the contract's implicit balance via the recipient account
                if tx.amount > 0 {
                    let recipient = accounts.entry(tx.to.clone()).or_default();
                    recipient.balance = recipient.balance.saturating_add(tx.amount);
                }
                receipts.insert(tx_hash, receipt);
            }
        }

        Ok(())
    }

    fn apply_coinbase_transaction(
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

    fn validate_transaction_shape(tx: &Transaction) -> Result<(), ChainError> {
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
                if tx.fee != 0 || tx.nonce != 0 || tx.signature.is_some() {
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
        }
    }

    fn apply_unstake_unlocks(accounts: &mut HashMap<Vec<u8>, AccountState>, block_height: u64) {
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

    fn remove_block_transactions_from_mempool(&mut self, block: &Block) {
        let included_hashes: HashSet<Vec<u8>> = block
            .transactions
            .iter()
            .filter(|tx| !tx.is_coinbase())
            .map(Transaction::hash)
            .collect();

        self.pending_transactions
            .retain(|pending| !included_hashes.contains(&pending.hash()));
    }

    fn persist_pending_transactions(&self) -> Result<(), ChainError> {
        if let Some(ref storage) = self.storage {
            storage.replace_pending_transactions(&self.pending_transactions)?;
            storage.flush()?;
        }
        Ok(())
    }

    fn persist_block_state(&self, block: &Block) -> Result<(), ChainError> {
        if let Some(ref storage) = self.storage {
            storage.put_block(block)?;
            for (address, state) in &self.accounts {
                storage.put_account(address, state)?;
            }
            storage.replace_pending_transactions(&self.pending_transactions)?;
            storage.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::dilithium::KeyPair;

    #[test]
    fn test_new_blockchain() {
        let chain = Blockchain::new();
        assert_eq!(chain.height(), 0);
        assert!(chain.is_valid());
    }

    #[test]
    fn test_custom_genesis_activates_validator() {
        let validator = KeyPair::generate();
        let chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "curs3d-test".to_string(),
            chain_name: "curs3d-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 100,
                staked_balance: 5_000,
            }],
            ..Default::default()
        })
        .unwrap();

        assert_eq!(chain.active_validator_count(), 1);
        assert_eq!(chain.genesis_config.chain_name, "curs3d-test");
    }

    #[test]
    fn test_create_and_add_block() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
        assert_eq!(chain.height(), 1);
        assert!(chain.is_valid());
    }

    #[test]
    fn test_transaction_flow() {
        let mut chain = Blockchain::new();
        let validator_kp = KeyPair::generate();
        let recipient = KeyPair::generate();

        let block = chain.create_block(&validator_kp).unwrap();
        chain.add_block(block).unwrap();

        let sender_address = hash::address_bytes_from_public_key(&validator_kp.public_key);
        let recipient_address = hash::address_bytes_from_public_key(&recipient.public_key);
        let mut tx = Transaction::new(
            chain.chain_id(),
            validator_kp.public_key.clone(),
            recipient_address.clone(),
            1000,
            10,
            0,
        );
        tx.sign(&validator_kp);
        chain.add_transaction(tx).unwrap();

        let block = chain.create_block(&validator_kp).unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(chain.get_balance(&recipient_address), 1000);
        assert_eq!(
            chain.get_balance(&sender_address),
            DEFAULT_BLOCK_REWARD * 2 - 1000
        );
        assert!(chain.is_valid());
    }

    #[test]
    fn test_rejects_forged_mint_transaction() {
        let mut chain = Blockchain::new();
        let attacker = KeyPair::generate();
        let victim = KeyPair::generate();

        let mut tx = Transaction::new(
            chain.chain_id(),
            attacker.public_key.clone(),
            hash::address_bytes_from_public_key(&victim.public_key),
            1_000,
            10,
            0,
        );
        tx.sign(&attacker);

        let err = chain.add_transaction(tx).unwrap_err();
        assert!(matches!(err, ChainError::InsufficientBalance { .. }));
    }

    #[test]
    fn test_stake_locks_funds() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let address = hash::address_bytes_from_public_key(&validator.public_key);

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let mut stake_tx =
            Transaction::stake(chain.chain_id(), validator.public_key.clone(), 10_000_000, 5, 0);
        stake_tx.sign(&validator);
        chain.add_transaction(stake_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(chain.get_staked_balance(&address), 10_000_000);
        assert_eq!(
            chain.get_balance(&address),
            DEFAULT_BLOCK_REWARD * 2 - 10_000_000
        );
    }

    #[test]
    fn test_rejects_duplicate_pending_transaction() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let mut tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            hash::address_bytes_from_public_key(&recipient.public_key),
            1000,
            10,
            0,
        );
        tx.sign(&validator);

        chain.add_transaction(tx.clone()).unwrap();
        let err = chain.add_transaction(tx).unwrap_err();
        assert!(matches!(err, ChainError::DuplicateTransaction));
    }

    #[test]
    fn test_unstake_unlocks_funds() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let address = hash::address_bytes_from_public_key(&validator.public_key);

        // Mine a block to get funds
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Stake 10M
        let mut stake_tx =
            Transaction::stake(chain.chain_id(), validator.public_key.clone(), 10_000_000, 5, 0);
        stake_tx.sign(&validator);
        chain.add_transaction(stake_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(chain.get_staked_balance(&address), 10_000_000);
        let balance_after_stake = chain.get_balance(&address);

        // Unstake 5M
        let mut unstake_tx = Transaction::unstake(
            chain.chain_id(),
            validator.public_key.clone(),
            5_000_000,
            5,
            1,
        );
        unstake_tx.sign(&validator);
        chain.add_transaction(unstake_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Staked reduced by 5M and funds remain locked until the unstake delay expires.
        assert_eq!(chain.get_staked_balance(&address), 5_000_000);
        assert_eq!(chain.get_account(&address).pending_unstakes.len(), 1);
        let balance_after_unstake_block = chain.get_balance(&address);
        assert_eq!(
            balance_after_unstake_block,
            balance_after_stake + DEFAULT_BLOCK_REWARD - 5_000_000
        );

        for _ in 0..DEFAULT_UNSTAKE_DELAY_BLOCKS {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        assert!(chain.get_balance(&address) >= balance_after_unstake_block + 5_000_000);
    }

    #[test]
    fn test_rejects_invalid_state_root() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        let mut block = chain.create_block(&validator).unwrap();
        block.header.state_root = vec![7; 32];
        block.hash = Block::compute_hash(&block.header);
        block.signature = Some(validator.sign(&block.hash));

        let err = chain.add_block(block).unwrap_err();
        assert!(matches!(err, ChainError::InvalidStateRoot));
    }

    #[test]
    fn test_rejects_unknown_finality_vote_hash() {
        let mut chain = Blockchain::new();
        let voter = KeyPair::generate();
        let vote = FinalityVote::new(hash::sha3_hash(b"unknown"), 1, &voter);
        assert!(chain.add_finality_vote(vote).is_none());
    }

    #[test]
    fn test_rejects_invalid_fork_state_root() {
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-fork-test".to_string(),
            chain_name: "curs3d-fork-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let mut fork = chain
            .create_block(&validator)
            .unwrap();
        fork.header.height = 1;
        fork.header.prev_hash = chain.blocks[0].hash.clone();
        fork.header.state_root = vec![7; 32];
        fork.hash = Block::compute_hash(&fork.header);
        fork.signature = Some(validator.sign(&fork.hash));

        let err = chain.add_block_with_fork_choice(fork).unwrap_err();
        assert!(matches!(err, ChainError::InvalidStateRoot));
    }

    #[test]
    fn test_deploy_contract() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        // Mine a block to get funds
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Deploy a contract with minimal valid WASM header
        let wasm_code = b"\0asm\x01\x00\x00\x00".to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            1_000_000,
            100,
            0,
        );
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Verify contract was stored and receipt exists
        assert_eq!(chain.contracts.len(), 1);
        assert!(!chain.receipts.is_empty());

        let receipt = chain.receipts.values().next().unwrap();
        assert!(receipt.success);
        assert!(receipt.contract_address.is_some());
        assert_eq!(receipt.contract_address.as_ref().unwrap().len(), 20);
        assert!(receipt.gas_used > 0);
    }

    #[test]
    fn test_call_contract() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        // Mine a block to get funds
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Deploy a contract
        let wasm_code = b"\0asm\x01\x00\x00\x00".to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            1_000_000,
            100,
            0,
        );
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Get the contract address from the receipt
        let contract_address = chain
            .receipts
            .values()
            .find(|r| r.contract_address.is_some())
            .unwrap()
            .contract_address
            .clone()
            .unwrap();

        // Call the contract
        let mut call_tx = Transaction::call_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            contract_address,
            b"do_something".to_vec(),
            0,
            1_000_000,
            100,
            1,
        );
        call_tx.sign(&validator);
        chain.add_transaction(call_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Should have 2 receipts now (deploy + call)
        assert_eq!(chain.receipts.len(), 2);
        let call_receipt = chain
            .receipts
            .values()
            .find(|r| r.contract_address.is_none())
            .unwrap();
        assert!(call_receipt.success);
        assert!(call_receipt.gas_used > 0);
    }

    #[test]
    fn test_gas_limit_exceeded() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        // Mine a block to get funds
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Try to deploy with gas_limit too low
        let wasm_code = b"\0asm\x01\x00\x00\x00".to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            100, // way too low
            100,
            0,
        );
        deploy_tx.sign(&validator);

        let err = chain.add_transaction(deploy_tx).unwrap_err();
        assert!(matches!(err, ChainError::VmError(_)));
    }

    #[test]
    fn test_epoch_snapshot_created_at_boundary() {
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-epoch-test".to_string(),
            chain_name: "curs3d-epoch-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: 4, // short epoch for testing
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        // Mine 4 blocks to hit epoch boundary
        for _ in 0..4 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        // After height 4, epoch 1 should have a snapshot
        assert!(chain.epoch_snapshots.contains_key(&1));
        let snapshot = chain.epoch_snapshots.get(&1).unwrap();
        assert_eq!(snapshot.epoch, 1);
        assert_eq!(snapshot.start_height, 4);
        assert!(!snapshot.validators.is_empty());
        assert!(snapshot.total_stake > 0);
    }

    #[test]
    fn test_validator_selection_uses_frozen_set() {
        use crate::consensus::ProofOfStake;

        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-frozen-test".to_string(),
            chain_name: "curs3d-frozen-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: 4,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        // Mine 4 blocks to create epoch snapshot
        for _ in 0..4 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        let snapshot = chain.epoch_snapshots.get(&1).unwrap();
        // The frozen set should contain the validator
        assert_eq!(snapshot.validators.len(), 1);
        assert_eq!(snapshot.validators[0].public_key, validator.public_key);

        // select_validator_from_snapshot should return the validator
        let selected = ProofOfStake::select_validator_from_snapshot(
            snapshot,
            5,
            &chain.latest_hash(),
        );
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().public_key, validator.public_key);
    }

    #[test]
    fn test_snapshot_create_and_verify() {
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-snapshot-test".to_string(),
            chain_name: "curs3d-snapshot-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis.clone()).unwrap();

        // Mine a few blocks
        for _ in 0..3 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        // Create a snapshot
        let manifest = chain.create_snapshot().unwrap();
        assert_eq!(manifest.height, 3);
        assert!(manifest.chunk_count > 0);
        assert_eq!(manifest.chunk_hashes.len(), manifest.chunk_count);

        // Verify state root matches
        let expected_root = Blockchain::compute_state_root(&chain.accounts);
        assert_eq!(manifest.state_root, expected_root);
    }

    #[test]
    fn test_protocol_version_at_height() {
        let genesis = GenesisConfig {
            chain_id: "curs3d-version-test".to_string(),
            chain_name: "curs3d-version-test".to_string(),
            upgrades: vec![
                super::ProtocolUpgrade {
                    height: 10,
                    version: 2,
                    description: "Version 2 upgrade".to_string(),
                },
                super::ProtocolUpgrade {
                    height: 20,
                    version: 3,
                    description: "Version 3 upgrade".to_string(),
                },
            ],
            ..Default::default()
        };
        let chain = Blockchain::from_genesis(genesis).unwrap();

        assert_eq!(chain.protocol_version_at_height(0), 1);
        assert_eq!(chain.protocol_version_at_height(5), 1);
        assert_eq!(chain.protocol_version_at_height(10), 2);
        assert_eq!(chain.protocol_version_at_height(15), 2);
        assert_eq!(chain.protocol_version_at_height(20), 3);
        assert_eq!(chain.protocol_version_at_height(100), 3);
    }

    #[test]
    fn test_rejects_wrong_protocol_version() {
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-ver-reject-test".to_string(),
            chain_name: "curs3d-ver-reject-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        // Create a valid block then tamper with its version
        let mut block = chain.create_block(&validator).unwrap();
        block.header.version = 99; // Wrong version
        block.hash = Block::compute_hash(&block.header);
        block.signature = Some(validator.sign(&block.hash));

        let err = chain.add_block(block).unwrap_err();
        assert!(matches!(err, ChainError::InvalidProtocolVersion { .. }));
    }

}
