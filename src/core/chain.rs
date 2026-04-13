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
pub const DEFAULT_INITIAL_BASE_FEE_PER_GAS: u64 = 0;
pub const DEFAULT_BASE_FEE_CHANGE_DENOMINATOR: u64 = 8;
const MAX_FUTURE_BLOCK_TIME_SECS: i64 = 30;
const MAX_FUTURE_TX_TIME_SECS: i64 = 30;
const MAX_PENDING_TX_AGE_SECS: i64 = 15 * 60;
const MAX_PENDING_TRANSACTIONS: usize = 10_000;
const MAX_PENDING_TRANSACTIONS_PER_ACCOUNT: usize = 64;
const MAX_PENDING_GAS_BUDGET_MULTIPLIER: u64 = 8;
const MIN_REPLACEMENT_FEE_BUMP_PCT: u64 = 10;
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
    #[serde(default = "default_initial_base_fee_per_gas")]
    pub initial_base_fee_per_gas: u64,
    #[serde(default = "default_base_fee_change_denominator")]
    pub base_fee_change_denominator: u64,
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

fn default_initial_base_fee_per_gas() -> u64 {
    DEFAULT_INITIAL_BASE_FEE_PER_GAS
}

fn default_base_fee_change_denominator() -> u64 {
    DEFAULT_BASE_FEE_CHANGE_DENOMINATOR
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
            initial_base_fee_per_gas: DEFAULT_INITIAL_BASE_FEE_PER_GAS,
            base_fee_change_denominator: DEFAULT_BASE_FEE_CHANGE_DENOMINATOR,
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
    #[error("replacement transaction fee bump too low")]
    ReplacementFeeTooLow,
    #[error("transaction fee too low for current mempool pressure")]
    FeeTooLow,
    #[error("invalid block base fee: expected {expected}, got {got}")]
    InvalidBaseFee { expected: u64, got: u64 },
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
    pub initial_base_fee_per_gas: u64,
    pub base_fee_change_denominator: u64,
    pub contracts: HashMap<Vec<u8>, ContractState>,
    pub receipts: HashMap<Vec<u8>, Receipt>,
    storage: Option<Storage>,
}

struct BlockExecution {
    accounts: HashMap<Vec<u8>, AccountState>,
    contracts: HashMap<Vec<u8>, ContractState>,
    receipts: HashMap<Vec<u8>, Receipt>,
}

impl Blockchain {
    pub fn new() -> Self {
        Self::from_genesis(GenesisConfig::default()).expect("default genesis must be valid")
    }

    pub fn from_genesis(genesis_config: GenesisConfig) -> Result<Self, ChainError> {
        let accounts = Self::accounts_from_genesis(&genesis_config)?;
        let contracts = HashMap::new();
        let state_root = Self::compute_state_root_full(&accounts, &contracts);
        let genesis = Block::genesis_with_state_root(
            state_root,
            &genesis_config.chain_id,
            genesis_config.initial_base_fee_per_gas,
        );
        let block_tree = BlockTree::from_genesis(&genesis);
        let mut epoch_snapshots = HashMap::new();
        epoch_snapshots.insert(
            0,
            Self::build_epoch_snapshot_for_accounts(
                &genesis_config,
                0,
                &accounts,
                &HashSet::new(),
            ),
        );

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
            initial_base_fee_per_gas: genesis_config.initial_base_fee_per_gas,
            base_fee_change_denominator: genesis_config.base_fee_change_denominator,
            genesis_config,
            block_tree,
            finality_tracker: FinalityTracker::new(),
            slashed_validators: HashSet::new(),
            epoch_snapshots,
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
                stored_genesis.initial_base_fee_per_gas,
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

            let pending_transactions =
                storage.get_all_pending_transactions_compat(&stored_genesis.chain_id)?;
            let slashed_validators = storage.get_slashed_addresses()?;
            let loaded_accounts_for_weights: HashMap<Vec<u8>, AccountState> = storage
                .get_all_accounts_compat()?
                .into_iter()
                .collect();

            // Rebuild block tree from stored blocks
            let block_tree = if !blocks.is_empty() {
                let mut tree = BlockTree::from_genesis(&blocks[0]);
                for block in blocks.iter().skip(1) {
                    let proposer_stake = loaded_accounts_for_weights
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
                storage.get_all_accounts_compat()?.len(),
                pending_transactions.len(),
                slashed_validators.len(),
                finalized_height,
            );

            let mut chain = Blockchain {
                blocks,
                accounts: HashMap::new(),
                pending_transactions,
                block_reward: stored_genesis.block_reward,
                minimum_stake: stored_genesis.minimum_stake,
                unstake_delay_blocks: stored_genesis.unstake_delay_blocks,
                epoch_length: stored_genesis.epoch_length,
                jail_duration_blocks: stored_genesis.jail_duration_blocks,
                block_gas_limit: stored_genesis.block_gas_limit,
                initial_base_fee_per_gas: stored_genesis.initial_base_fee_per_gas,
                base_fee_change_denominator: stored_genesis.base_fee_change_denominator,
                genesis_config: stored_genesis,
                block_tree,
                finality_tracker,
                slashed_validators,
                epoch_snapshots: HashMap::new(),
                contracts: HashMap::new(),
                receipts: HashMap::new(),
                storage: Some(storage),
            };

            chain.rebuild_canonical_state()?;

            if schema_version < crate::storage::CURRENT_SCHEMA_VERSION {
                chain.persist_full_state()?;
            }

            Ok(chain)
        } else {
            let mut chain = Self::from_genesis(genesis_config.cloned().unwrap_or_default())?;
            chain.storage = Some(storage);
            chain.persist_full_state()?;

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

    #[allow(dead_code)]
    pub fn current_base_fee_per_gas(&self) -> u64 {
        self.latest_block().header.base_fee_per_gas
    }

    fn target_block_gas_usage(&self) -> u64 {
        (self.block_gas_limit / 2).max(1)
    }

    fn next_base_fee_per_gas(&self, parent: &Block) -> u64 {
        let parent_base_fee = if parent.header.height == 0 {
            parent.header.base_fee_per_gas.max(self.initial_base_fee_per_gas)
        } else {
            parent.header.base_fee_per_gas
        };
        let target = self.target_block_gas_usage();
        if parent.header.gas_used == target {
            return parent_base_fee;
        }
        if parent_base_fee == 0 && parent.header.gas_used <= target {
            return 0;
        }

        let delta = if parent.header.gas_used > target {
            parent.header.gas_used - target
        } else {
            target - parent.header.gas_used
        };
        let change = parent_base_fee
            .max(1)
            .saturating_mul(delta)
            .checked_div(target.saturating_mul(self.base_fee_change_denominator.max(1)))
            .unwrap_or(0)
            .max(1);

        if parent.header.gas_used > target {
            parent_base_fee.saturating_add(change)
        } else {
            parent_base_fee.saturating_sub(change)
        }
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

    fn build_epoch_snapshot_for_accounts(
        genesis_config: &GenesisConfig,
        epoch: u64,
        accounts: &HashMap<Vec<u8>, AccountState>,
        slashed_validators: &HashSet<Vec<u8>>,
    ) -> EpochSnapshot {
        let start_height = epoch * genesis_config.epoch_length.max(1);
        let pos = ProofOfStake::with_slashed(
            genesis_config.minimum_stake,
            slashed_validators.clone(),
            start_height,
        );
        let validators = pos.active_validators(accounts);
        let total_stake: u64 = validators.iter().map(|v| v.stake).sum();
        EpochSnapshot {
            epoch,
            start_height,
            validators,
            total_stake,
        }
    }

    fn snapshot_for_accounts(&self, epoch: u64, accounts: &HashMap<Vec<u8>, AccountState>) -> EpochSnapshot {
        Self::build_epoch_snapshot_for_accounts(
            &self.genesis_config,
            epoch,
            accounts,
            &self.slashed_validators,
        )
    }

    /// Compute and store an EpochSnapshot for the given epoch using the provided pre-epoch state.
    pub fn create_epoch_snapshot_from_accounts(
        &mut self,
        epoch: u64,
        accounts: &HashMap<Vec<u8>, AccountState>,
    ) {
        let snapshot = self.snapshot_for_accounts(epoch, accounts);
        self.epoch_snapshots.insert(epoch, snapshot);
    }

    /// Get the EpochSnapshot for a given epoch, if it exists.
    #[allow(dead_code)]
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
        let snapshot_height = if self.finality_tracker.finalized_height > 0 {
            self.finality_tracker.finalized_height.min(self.height())
        } else {
            self.height()
        };
        let snapshot_hash = self
            .blocks
            .get(snapshot_height as usize)
            .map(|block| block.hash.clone())
            .ok_or_else(|| ChainError::SnapshotError("snapshot height missing".to_string()))?;
        let (snapshot_accounts, snapshot_contracts, snapshot_receipts) =
            self.replay_state_to_canonical_height(snapshot_height)?;
        let mut accounts: Vec<(Vec<u8>, AccountState)> = snapshot_accounts
            .iter()
            .map(|(address, state)| (address.clone(), state.clone()))
            .collect();
        accounts.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut contracts: Vec<(Vec<u8>, ContractState)> = snapshot_contracts
            .iter()
            .map(|(address, state)| (address.clone(), state.clone()))
            .collect();
        contracts.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut receipts: Vec<(Vec<u8>, Receipt)> = snapshot_receipts
            .iter()
            .map(|(tx_hash, receipt)| (tx_hash.clone(), receipt.clone()))
            .collect();
        receipts.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut epoch_snapshots: Vec<(u64, EpochSnapshot)> = self
            .epoch_snapshots
            .iter()
            .filter(|(epoch, _)| **epoch <= self.epoch_for_height(snapshot_height))
            .map(|(epoch, snapshot)| (*epoch, snapshot.clone()))
            .collect();
        epoch_snapshots.sort_by_key(|(epoch, _)| *epoch);

        let snapshot_state = crate::storage::SnapshotState {
            blocks: self.blocks[..=snapshot_height as usize].to_vec(),
            accounts,
            contracts,
            receipts,
            pending_transactions: Vec::new(),
            slashed_validators: self.slashed_validators.iter().cloned().collect(),
            epoch_snapshots,
            finalized_height: snapshot_height,
            finalized_hash: snapshot_hash.clone(),
        };

        let snapshot_bytes = bincode::serialize(&snapshot_state)
            .map_err(|e| ChainError::SnapshotError(e.to_string()))?;

        let chunk_size = 256 * 1024;
        let mut chunks = Vec::new();
        let mut chunk_hashes = Vec::new();
        for (index, data) in snapshot_bytes.chunks(chunk_size).enumerate() {
            let chunk_hash = hash::sha3_hash(data);
            chunk_hashes.push(chunk_hash.clone());
            chunks.push((index, data.to_vec(), chunk_hash));
        }
        let chunk_root = hash::merkle_root(&chunk_hashes);
        let mut chunks: Vec<crate::storage::StateChunk> = chunks
            .into_iter()
            .map(|(index, data, chunk_hash)| crate::storage::StateChunk {
                index,
                data,
                hash: chunk_hash,
                proof: hash::merkle_proof(&chunk_hashes, index),
            })
            .collect();
        if chunks.is_empty() {
            chunks.push(crate::storage::StateChunk {
                index: 0,
                data: Vec::new(),
                hash: hash::sha3_hash(&[]),
                proof: Vec::new(),
            });
        }

        let epoch = self.epoch_for_height(snapshot_height);
        let state_root = Self::compute_state_root_full(&snapshot_accounts, &snapshot_contracts);
        let manifest = crate::storage::SnapshotManifest {
            height: snapshot_height,
            epoch,
            chain_id: self.chain_id().to_string(),
            genesis_hash: self.genesis_hash().to_vec(),
            latest_hash: snapshot_hash.clone(),
            tip_height: self.height(),
            tip_hash: self.latest_hash().to_vec(),
            finalized_height: snapshot_height,
            finalized_hash: snapshot_hash,
            state_root,
            chunk_root,
            chunk_count: chunks.len(),
            chunk_hashes,
        };

        // Persist chunks to storage
        if let Some(ref storage) = self.storage {
            for chunk in &chunks {
                let _ = storage.put_snapshot_chunk(snapshot_height, chunk);
            }
            let _ = storage.put_snapshot_manifest(snapshot_height, &manifest);
        }

        Ok(manifest)
    }

    pub fn get_snapshot_chunks(
        &self,
        height: u64,
    ) -> Result<Vec<crate::storage::StateChunk>, ChainError> {
        if let Some(ref storage) = self.storage {
            return storage.get_snapshot_chunks(height).map_err(ChainError::from);
        }
        Err(ChainError::SnapshotError(
            "snapshot chunks unavailable without storage backend".to_string(),
        ))
    }

    fn decode_snapshot_state(
        manifest: &crate::storage::SnapshotManifest,
        chunks: &[crate::storage::StateChunk],
    ) -> Result<crate::storage::SnapshotState, ChainError> {
        if chunks.len() != manifest.chunk_count {
            return Err(ChainError::SnapshotError(format!(
                "expected {} chunks, got {}",
                manifest.chunk_count,
                chunks.len()
            )));
        }
        for (i, chunk) in chunks.iter().enumerate() {
            if chunk.index != i {
                return Err(ChainError::SnapshotError(format!(
                    "unexpected chunk order: expected {}, got {}",
                    i, chunk.index
                )));
            }
            let computed_hash = hash::sha3_hash(&chunk.data);
            if i >= manifest.chunk_hashes.len() || computed_hash != manifest.chunk_hashes[i] {
                return Err(ChainError::SnapshotError(format!(
                    "chunk {} hash mismatch",
                    i
                )));
            }
            if !hash::verify_merkle_proof(&computed_hash, &chunk.proof, i, &manifest.chunk_root) {
                return Err(ChainError::SnapshotError(format!(
                    "chunk {} proof mismatch",
                    i
                )));
            }
        }

        let payload: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.data.clone()).collect();
        let snapshot_state: crate::storage::SnapshotState = bincode::deserialize(&payload)
            .map_err(|e| ChainError::SnapshotError(e.to_string()))?;

        let accounts: HashMap<Vec<u8>, AccountState> =
            snapshot_state.accounts.iter().cloned().collect();
        let contracts: HashMap<Vec<u8>, ContractState> =
            snapshot_state.contracts.iter().cloned().collect();
        let computed_root = Self::compute_state_root_full(&accounts, &contracts);
        if computed_root != manifest.state_root {
            return Err(ChainError::SnapshotError(
                "state root mismatch".to_string(),
            ));
        }

        if snapshot_state.blocks.is_empty() {
            return Err(ChainError::SnapshotError(
                "snapshot does not contain canonical blocks".to_string(),
            ));
        }
        if snapshot_state.blocks[0].hash != manifest.genesis_hash {
            return Err(ChainError::SnapshotError(
                "snapshot genesis hash mismatch".to_string(),
            ));
        }
        if snapshot_state
            .blocks
            .last()
            .map(|block| block.hash.clone())
            .unwrap_or_default()
            != manifest.latest_hash
        {
            return Err(ChainError::SnapshotError(
                "snapshot latest hash mismatch".to_string(),
            ));
        }

        Ok(snapshot_state)
    }

    pub fn apply_snapshot(
        &mut self,
        manifest: &crate::storage::SnapshotManifest,
        chunks: &[crate::storage::StateChunk],
    ) -> Result<(), ChainError> {
        if manifest.chain_id != self.chain_id() {
            return Err(ChainError::SnapshotError(
                "snapshot chain_id mismatch".to_string(),
            ));
        }
        if self.finality_tracker.finalized_height >= manifest.height
            && !self.finality_tracker.finalized_hash.is_empty()
            && self.finality_tracker.finalized_height == manifest.finalized_height
            && self.finality_tracker.finalized_hash != manifest.finalized_hash
        {
            return Err(ChainError::SnapshotError(
                "snapshot finalized hash conflicts with local finalized checkpoint".to_string(),
            ));
        }
        if let Some(local_block) = self.blocks.get(manifest.height as usize)
            && local_block.hash != manifest.latest_hash
        {
            return Err(ChainError::SnapshotError(
                "snapshot latest hash conflicts with local canonical block".to_string(),
            ));
        }

        let snapshot_state = Self::decode_snapshot_state(manifest, chunks)?;

        self.blocks = snapshot_state.blocks;
        self.accounts = snapshot_state.accounts.into_iter().collect();
        self.contracts = snapshot_state.contracts.into_iter().collect();
        self.receipts = snapshot_state.receipts.into_iter().collect();
        self.pending_transactions = snapshot_state.pending_transactions;
        self.slashed_validators = snapshot_state.slashed_validators.into_iter().collect();
        self.epoch_snapshots = snapshot_state.epoch_snapshots.into_iter().collect();

        let mut block_tree = BlockTree::from_genesis(
            self.blocks
                .first()
                .ok_or_else(|| ChainError::SnapshotError("missing genesis block".to_string()))?,
        );
        for block in self.blocks.iter().skip(1) {
            let proposer_address =
                hash::address_bytes_from_public_key(&block.header.validator_public_key);
            let proposer_stake = self
                .accounts
                .get(&proposer_address)
                .map(|account| account.staked_balance)
                .unwrap_or(0);
            let _ = block_tree.insert(block.clone(), proposer_stake);
        }
        block_tree.set_finalized(
            manifest.finalized_hash.clone(),
            manifest.finalized_height,
        );
        self.block_tree = block_tree;
        self.finality_tracker =
            FinalityTracker::with_finalized(manifest.finalized_height, manifest.finalized_hash.clone());

        self.persist_full_state()?;
        Ok(())
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
        self.prune_pending_transactions();

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

        let now = chrono::Utc::now().timestamp();
        if tx.timestamp > now + MAX_FUTURE_TX_TIME_SECS {
            return Err(ChainError::InvalidTransactionFormat(
                "transaction timestamp too far in the future",
            ));
        }
        let pending_base_fee = self.next_base_fee_per_gas(self.latest_block());
        if tx.fee < self.minimum_admission_fee(&tx, pending_base_fee) {
            return Err(ChainError::FeeTooLow);
        }

        let replacement_index = self
            .pending_transactions
            .iter()
            .position(|pending| pending.from == tx.from && pending.nonce == tx.nonce);

        if self.pending_transactions.len() >= MAX_PENDING_TRANSACTIONS && replacement_index.is_none() {
            return Err(ChainError::MempoolFull);
        }

        let sender_pending = self
            .pending_transactions
            .iter()
            .filter(|pending| pending.from == tx.from)
            .count();
        if sender_pending >= MAX_PENDING_TRANSACTIONS_PER_ACCOUNT && replacement_index.is_none() {
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

        if let Some(index) = replacement_index {
            let existing = &self.pending_transactions[index];
            let min_fee = existing
                .fee
                .saturating_add(existing.fee.saturating_mul(MIN_REPLACEMENT_FEE_BUMP_PCT) / 100)
                .max(existing.fee.saturating_add(1));
            if tx.fee < min_fee {
                return Err(ChainError::ReplacementFeeTooLow);
            }
        }

        let mut projected_accounts = self.accounts.clone();
        let mut projected_contracts = self.contracts.clone();
        let mut projected_receipts = HashMap::new();
        let mut seen_hashes = HashSet::new();
        for (index, pending) in self.pending_transactions.iter().enumerate() {
            if replacement_index == Some(index) {
                continue;
            }
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
                pending_base_fee,
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
            pending_base_fee,
        )?;
        let protected_from = tx.from.clone();
        let protected_nonce = tx.nonce;
        if let Some(index) = replacement_index {
            self.pending_transactions[index] = tx;
        } else {
            self.pending_transactions.push(tx);
        }
        let protected_hash = self
            .pending_transactions
            .iter()
            .find(|pending| pending.from == protected_from && pending.nonce == protected_nonce)
            .map(Transaction::hash)
            .unwrap_or_default();
        self.enforce_mempool_limits(&protected_hash)?;
        self.sort_pending_transactions();
        self.persist_pending_transactions()?;
        Ok(())
    }

    pub fn create_block(&self, validator_keypair: &KeyPair) -> Result<Block, ChainError> {
        let prev_block = self.latest_block();
        let height = prev_block.header.height + 1;
        let prev_hash = prev_block.hash.clone();
        let protocol_version = self.protocol_version_at_height(height);
        let base_fee_per_gas = self.next_base_fee_per_gas(prev_block);

        let proposer_public_key = validator_keypair.public_key.clone();
        let proposer_address = hash::address_bytes_from_public_key(&proposer_public_key);
        self.ensure_validator_is_authorized(&proposer_public_key, height, &prev_hash)?;

        let mut projected_accounts = self.accounts.clone();
        let mut projected_contracts = self.contracts.clone();
        let mut projected_receipts = HashMap::new();
        Self::apply_unstake_unlocks(&mut projected_accounts, height);
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
                pending,
                height,
                self.unstake_delay_blocks,
                self.epoch_length,
                self.minimum_stake,
                base_fee_per_gas,
            ) {
                Ok(gas_used) if total_gas_used.saturating_add(gas_used) <= self.block_gas_limit => {
                    total_gas_used = total_gas_used.saturating_add(gas_used);
                    total_priority_fees = total_priority_fees
                        .saturating_add(Self::priority_fee_for_transaction(pending, gas_used, base_fee_per_gas));
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

        Ok(Block::new(
            protocol_version,
            height,
            prev_hash,
            Self::compute_state_root_full(&projected_accounts, &projected_contracts),
            total_gas_used,
            base_fee_per_gas,
            transactions,
            validator_keypair,
        ))
    }

    pub fn add_block(&mut self, block: Block) -> Result<(), ChainError> {
        let prev_accounts = self.accounts.clone();
        if block.header.height > 0 && block.header.height % self.epoch_length.max(1) == 0 {
            let epoch = self.epoch_for_height(block.header.height);
            if !self.epoch_snapshots.contains_key(&epoch) {
                self.create_epoch_snapshot_from_accounts(epoch, &prev_accounts);
            }
        }
        let prev = self.latest_block();
        let execution =
            self.validate_block_against_state(&block, prev, &self.accounts, &self.contracts)?;

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
        self.blocks.push(block.clone());
        self.remove_block_transactions_from_mempool(&block);
        self.persist_full_state()?;

        Ok(())
    }

    /// Add a finality vote. Returns Some(FinalizedBlock) if threshold reached.
    pub fn add_finality_vote(&mut self, vote: FinalityVote) -> Option<FinalizedBlock> {
        let voted_block = self.block_tree.get(&vote.block_hash)?;
        if voted_block.header.height != vote.block_height {
            return None;
        }
        let vote_epoch = self.epoch_for_height(vote.block_height);
        if vote.epoch != vote_epoch {
            return None;
        }
        if !self.block_tree.is_on_canonical_chain(&vote.block_hash) {
            return None;
        }
        let snapshot = self.epoch_snapshots.get(&vote_epoch)?;

        let result = self.finality_tracker.add_vote(vote, snapshot);

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

        replay.accounts == self.accounts && replay.contracts == self.contracts
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
        let (parent_accounts, parent_contracts, _) = self.replay_state_to_tip(&parent.hash)?;
        let execution =
            self.validate_block_against_state(&block, &parent, &parent_accounts, &parent_contracts)?;

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

        self.blocks = canonical.iter().cloned().cloned().collect();
        self.rebuild_canonical_state()?;
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
            storage.put_meta(CHAIN_CONFIG_KEY, &self.genesis_config)?;
            storage.put_meta(b"finalized_height", &self.finality_tracker.finalized_height)?;
            storage.put_meta(crate::storage::SCHEMA_VERSION_KEY, &crate::storage::CURRENT_SCHEMA_VERSION)?;
            storage.replace_blocks(&self.blocks)?;
            storage.replace_accounts(&self.accounts)?;
            storage.replace_contracts(&self.contracts)?;
            storage.replace_receipts(&self.receipts)?;
            storage.replace_epoch_snapshots(&self.epoch_snapshots)?;
            storage.replace_pending_transactions(&self.pending_transactions)?;
            storage.flush()?;
        }
        Ok(())
    }

    #[allow(dead_code)]
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
            &self.epoch_snapshots,
            validator_public_key,
            block_height,
            prev_hash,
        )
    }

    fn ensure_validator_is_authorized_for_accounts(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
        epoch_snapshots: &HashMap<u64, EpochSnapshot>,
        validator_public_key: &[u8],
        block_height: u64,
        prev_hash: &[u8],
    ) -> Result<(), ChainError> {
        // Try to use frozen epoch snapshot for validator selection
        let epoch = block_height / self.epoch_length.max(1);
        if let Some(snapshot) = epoch_snapshots.get(&epoch) {
            match ProofOfStake::select_validator_from_snapshot(snapshot, block_height, prev_hash) {
                Some(expected) if expected.public_key == validator_public_key => return Ok(()),
                Some(_) => return Err(ChainError::UnauthorizedValidator),
                None => return Ok(()),
            }
        }

        if epoch > 0 && block_height % self.epoch_length.max(1) == 0 {
            let snapshot = self.snapshot_for_accounts(epoch, accounts);
            match ProofOfStake::select_validator_from_snapshot(&snapshot, block_height, prev_hash) {
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
        parent_contracts: &HashMap<Vec<u8>, ContractState>,
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

        self.ensure_validator_is_authorized_for_accounts(
            parent_accounts,
            &self.epoch_snapshots,
            &block.header.validator_public_key,
            block.header.height,
            &block.header.prev_hash,
        )?;

        let proposer_address =
            hash::address_bytes_from_public_key(&block.header.validator_public_key);
        let mut projected_accounts = parent_accounts.clone();
        let mut projected_contracts = parent_contracts.clone();
        let mut projected_receipts = HashMap::new();
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

        if block.header.state_root
            != Self::compute_state_root_full(&projected_accounts, &projected_contracts)
        {
            return Err(ChainError::InvalidStateRoot);
        }

        Ok(BlockExecution {
            accounts: projected_accounts,
            contracts: projected_contracts,
            receipts: projected_receipts,
        })
    }

    fn replay_state_to_tip(
        &self,
        tip_hash: &[u8],
    ) -> Result<
        (
            HashMap<Vec<u8>, AccountState>,
            HashMap<Vec<u8>, ContractState>,
            HashMap<Vec<u8>, Receipt>,
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
        let mut contracts = HashMap::new();
        let mut receipts = HashMap::new();
        let mut previous = lineage
            .first()
            .cloned()
            .expect("lineage always includes genesis");
        for block in lineage.iter().skip(1) {
            let execution =
                self.validate_block_against_state(block, &previous, &accounts, &contracts)?;
            accounts = execution.accounts;
            contracts = execution.contracts;
            receipts.extend(execution.receipts);
            previous = block.clone();
        }

        Ok((accounts, contracts, receipts))
    }

    fn replay_state_to_canonical_height(
        &self,
        target_height: u64,
    ) -> Result<
        (
            HashMap<Vec<u8>, AccountState>,
            HashMap<Vec<u8>, ContractState>,
            HashMap<Vec<u8>, Receipt>,
        ),
        ChainError,
    > {
        let block = self
            .blocks
            .get(target_height as usize)
            .ok_or_else(|| ChainError::SnapshotError("target height missing".to_string()))?;
        self.replay_state_to_tip(&block.hash)
    }

    fn ensure_transaction_fee_covers_base(
        tx: &Transaction,
        gas_used: u64,
        base_fee_per_gas: u64,
    ) -> Result<(), ChainError> {
        let required = gas_used.saturating_mul(base_fee_per_gas);
        if tx.fee < required {
            return Err(ChainError::FeeTooLow);
        }
        Ok(())
    }

    fn priority_fee_for_transaction(
        tx: &Transaction,
        gas_used: u64,
        base_fee_per_gas: u64,
    ) -> u64 {
        tx.fee
            .saturating_sub(gas_used.saturating_mul(base_fee_per_gas))
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
        base_fee_per_gas: u64,
    ) -> Result<u64, ChainError> {
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
                let gas_used = crate::vm::gas::GAS_BASE_TX;
                Self::ensure_transaction_fee_covers_base(tx, gas_used, base_fee_per_gas)?;
                Ok(gas_used)
            }
            TransactionKind::Stake => {
                let gas_used = crate::vm::gas::GAS_BASE_TX;
                Self::ensure_transaction_fee_covers_base(tx, gas_used, base_fee_per_gas)?;
                Ok(gas_used)
            }
            TransactionKind::Unstake => {
                let gas_used = crate::vm::gas::GAS_BASE_TX;
                Self::ensure_transaction_fee_covers_base(tx, gas_used, base_fee_per_gas)?;
                Ok(gas_used)
            }
            TransactionKind::Coinbase => {
                Err(ChainError::InvalidTransactionFormat(
                    "coinbase not allowed in user transaction flow",
                ))
            }
            TransactionKind::DeployContract => {
                let (contract, mut receipt) =
                    Vm::deploy(&tx.to, &tx.from, tx.nonce.wrapping_sub(1), tx.gas_limit)?;
                let gas_used = receipt.gas_used;
                receipt.tx_hash = tx_hash.clone();
                if let Some(ref addr) = receipt.contract_address {
                    contracts.insert(addr.clone(), contract);
                }
                Self::ensure_transaction_fee_covers_base(tx, gas_used, base_fee_per_gas)?;
                receipts.insert(tx_hash, receipt);
                Ok(gas_used)
            }
            TransactionKind::CallContract => {
                let contract = contracts.get_mut(&tx.to).ok_or_else(|| {
                    ChainError::ContractNotFound(hex::encode(&tx.to))
                })?;
                let mut receipt =
                    Vm::call(contract, &tx.data, &tx.from, tx.amount, tx.gas_limit)?;
                let gas_used = receipt.gas_used;
                receipt.tx_hash = tx_hash.clone();
                // Credit the contract's implicit balance via the recipient account
                if tx.amount > 0 {
                    let recipient = accounts.entry(tx.to.clone()).or_default();
                    recipient.balance = recipient.balance.saturating_add(tx.amount);
                }
                Self::ensure_transaction_fee_covers_base(tx, gas_used, base_fee_per_gas)?;
                receipts.insert(tx_hash, receipt);
                Ok(gas_used)
            }
        }
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

    fn sort_pending_transactions(&mut self) {
        let base_fee_per_gas = self.next_base_fee_per_gas(self.latest_block());
        self.pending_transactions.sort_by(|a, b| {
            if a.from == b.from {
                a.nonce.cmp(&b.nonce).then_with(|| b.fee.cmp(&a.fee))
            } else {
                Self::compare_fee_priority(a, b, base_fee_per_gas)
                    .then_with(|| a.timestamp.cmp(&b.timestamp))
                    .then_with(|| b.fee.cmp(&a.fee))
            }
        });
    }

    fn prune_pending_transactions(&mut self) {
        let cutoff = chrono::Utc::now().timestamp() - MAX_PENDING_TX_AGE_SECS;
        self.pending_transactions
            .retain(|pending| pending.timestamp >= cutoff);
        self.sort_pending_transactions();
    }

    fn compare_fee_priority(
        a: &Transaction,
        b: &Transaction,
        base_fee_per_gas: u64,
    ) -> std::cmp::Ordering {
        let a_gas = a.estimated_gas_for_admission().max(1) as u128;
        let b_gas = b.estimated_gas_for_admission().max(1) as u128;
        let a_fee = a
            .fee
            .saturating_sub(a.estimated_gas_for_admission().saturating_mul(base_fee_per_gas))
            as u128;
        let b_fee = b
            .fee
            .saturating_sub(b.estimated_gas_for_admission().saturating_mul(base_fee_per_gas))
            as u128;
        (b_fee.saturating_mul(a_gas))
            .cmp(&a_fee.saturating_mul(b_gas))
            .then_with(|| b.fee.cmp(&a.fee))
    }

    fn pending_gas_budget(&self) -> u64 {
        self.block_gas_limit
            .saturating_mul(MAX_PENDING_GAS_BUDGET_MULTIPLIER)
    }

    fn pending_gas_usage(&self) -> u64 {
        self.pending_transactions
            .iter()
            .map(Transaction::estimated_gas_for_admission)
            .sum()
    }

    fn minimum_admission_fee(&self, tx: &Transaction, base_fee_per_gas: u64) -> u64 {
        let required_base = tx
            .estimated_gas_for_admission()
            .saturating_mul(base_fee_per_gas);
        let usage = self.pending_gas_usage();
        let budget = self.pending_gas_budget().max(1);
        let occupancy_pct = usage.saturating_mul(100) / budget;
        let surcharge = if occupancy_pct >= 95 {
            8
        } else if occupancy_pct >= 85 {
            4
        } else if occupancy_pct >= 70 {
            2
        } else if occupancy_pct >= 50 {
            1
        } else {
            0
        };
        if surcharge == 0 {
            return required_base;
        }
        let units = tx
            .estimated_gas_for_admission()
            .saturating_add(99_999)
            / 100_000;
        required_base.saturating_add(units.max(1).saturating_mul(surcharge))
    }

    fn worst_pending_transaction_index(&self) -> Option<usize> {
        let base_fee_per_gas = self.next_base_fee_per_gas(self.latest_block());
        self.pending_transactions
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                if a.from == b.from {
                    b.nonce.cmp(&a.nonce).then_with(|| a.fee.cmp(&b.fee))
                } else {
                    Self::compare_fee_priority(a, b, base_fee_per_gas).reverse()
                }
            })
            .map(|(index, _)| index)
    }

    fn evict_transaction_and_dependents(&mut self, index: usize) {
        if index >= self.pending_transactions.len() {
            return;
        }
        let evicted = self.pending_transactions.remove(index);
        self.pending_transactions.retain(|pending| {
            !(pending.from == evicted.from && pending.nonce > evicted.nonce)
        });
    }

    fn enforce_mempool_limits(&mut self, protected_hash: &[u8]) -> Result<(), ChainError> {
        loop {
            let over_count = self.pending_transactions.len() > MAX_PENDING_TRANSACTIONS;
            let over_gas = self.pending_gas_usage() > self.pending_gas_budget();
            if !over_count && !over_gas {
                break;
            }

            let Some(index) = self.worst_pending_transaction_index() else {
                break;
            };
            let is_protected = self.pending_transactions[index].hash() == protected_hash;
            if is_protected {
                if over_gas {
                    return Err(ChainError::FeeTooLow);
                }
                return Err(ChainError::MempoolFull);
            }
            self.evict_transaction_and_dependents(index);
        }
        Ok(())
    }

    fn persist_pending_transactions(&self) -> Result<(), ChainError> {
        if let Some(ref storage) = self.storage {
            storage.replace_pending_transactions(&self.pending_transactions)?;
            storage.flush()?;
        }
        Ok(())
    }

    fn rebuild_canonical_state(&mut self) -> Result<(), ChainError> {
        let blocks = self.blocks.clone();
        let mut accounts = Self::accounts_from_genesis(&self.genesis_config)?;
        let mut contracts = HashMap::new();
        let mut receipts = HashMap::new();
        self.epoch_snapshots.clear();
        self.epoch_snapshots.insert(0, self.snapshot_for_accounts(0, &accounts));

        let mut previous = blocks
            .first()
            .cloned()
            .ok_or_else(|| ChainError::InvalidGenesis("missing genesis block".to_string()))?;
        for block in blocks.iter().skip(1) {
            if block.header.height > 0 && block.header.height % self.epoch_length.max(1) == 0 {
                let epoch = self.epoch_for_height(block.header.height);
                if !self.epoch_snapshots.contains_key(&epoch) {
                    self.create_epoch_snapshot_from_accounts(epoch, &accounts);
                }
            }
            let execution =
                self.validate_block_against_state(block, &previous, &accounts, &contracts)?;
            accounts = execution.accounts;
            contracts = execution.contracts;
            receipts.extend(execution.receipts);
            previous = block.clone();
        }

        self.accounts = accounts;
        self.contracts = contracts;
        self.receipts = receipts;
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
    fn test_base_fee_rises_after_busy_block() {
        let validator = KeyPair::generate();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "base-fee-test".to_string(),
            chain_name: "base-fee-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            block_gas_limit: 100_000,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        })
        .unwrap();

        let wasm_code = br#"(module
            (memory (export "memory") 1)
            (func (export "curs3d_call"))
        )"#
        .to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            80_000,
            100_000,
            0,
        );
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        assert_eq!(chain.current_base_fee_per_gas(), 0);
        let block1 = chain.create_block(&validator).unwrap();
        assert_eq!(block1.header.base_fee_per_gas, 0);
        assert!(block1.header.gas_used > chain.target_block_gas_usage());
        chain.add_block(block1).unwrap();

        let block2 = chain.create_block(&validator).unwrap();
        assert!(block2.header.base_fee_per_gas > 0);
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
    fn test_mempool_evicts_low_fee_under_gas_pressure() {
        let kp1 = KeyPair::generate();
        let kp2 = KeyPair::generate();
        let kp3 = KeyPair::generate();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "mempool-pressure-test".to_string(),
            chain_name: "mempool-pressure-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            block_gas_limit: 100_000,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&kp1.public_key),
                    balance: 10_000_000,
                    staked_balance: 0,
                },
                GenesisAllocation {
                    public_key: hex::encode(&kp2.public_key),
                    balance: 10_000_000,
                    staked_balance: 0,
                },
                GenesisAllocation {
                    public_key: hex::encode(&kp3.public_key),
                    balance: 10_000_000,
                    staked_balance: 0,
                },
            ],
            ..Default::default()
        })
        .unwrap();

        let wasm_code = br#"(module (memory (export "memory") 1) (func (export "curs3d_call")))"#
            .to_vec();
        let mut tx1 = Transaction::deploy_contract(
            chain.chain_id(),
            kp1.public_key.clone(),
            wasm_code.clone(),
            400_000,
            5,
            0,
        );
        tx1.sign(&kp1);
        chain.add_transaction(tx1.clone()).unwrap();

        let mut tx2 = Transaction::deploy_contract(
            chain.chain_id(),
            kp2.public_key.clone(),
            wasm_code.clone(),
            400_000,
            5,
            0,
        );
        tx2.sign(&kp2);
        chain.add_transaction(tx2.clone()).unwrap();

        let mut tx3 = Transaction::deploy_contract(
            chain.chain_id(),
            kp3.public_key.clone(),
            wasm_code,
            400_000,
            50,
            0,
        );
        tx3.sign(&kp3);
        chain.add_transaction(tx3.clone()).unwrap();

        assert_eq!(chain.pending_transactions.len(), 2);
        assert!(chain
            .pending_transactions
            .iter()
            .any(|pending| pending.hash() == tx3.hash()));
        assert!(chain.pending_gas_usage() <= chain.pending_gas_budget());
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
        let vote = FinalityVote::new(hash::sha3_hash(b"unknown"), 1, 0, &voter);
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

        let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 7)
        )"#
        .to_vec();
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

        let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 7)
        )"#
        .to_vec();
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
        assert_eq!(call_receipt.return_data, 7i32.to_le_bytes().to_vec());
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
        let dir = tempfile::tempdir().unwrap();
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
        let mut chain = Blockchain::with_storage(
            dir.path().to_str().unwrap(),
            Some(&genesis),
        )
        .unwrap();

        // Mine a few blocks
        for _ in 0..3 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 9)
        )"#
        .to_vec();
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

        // Create a snapshot
        let manifest = chain.create_snapshot().unwrap();
        let chunks = chain.get_snapshot_chunks(manifest.height).unwrap();
        assert_eq!(manifest.height, 4);
        assert!(manifest.chunk_count > 0);
        assert_eq!(manifest.chunk_hashes.len(), manifest.chunk_count);

        // Verify state root matches
        let expected_root = Blockchain::compute_state_root_full(&chain.accounts, &chain.contracts);
        assert_eq!(manifest.state_root, expected_root);

        let mut restored = Blockchain::from_genesis(genesis).unwrap();
        restored.apply_snapshot(&manifest, &chunks).unwrap();
        assert_eq!(restored.accounts, chain.accounts);
        assert_eq!(restored.contracts, chain.contracts);
        assert_eq!(restored.receipts.len(), chain.receipts.len());
        for (tx_hash, receipt) in &chain.receipts {
            let restored_receipt = restored.receipts.get(tx_hash).unwrap();
            assert_eq!(restored_receipt.success, receipt.success);
            assert_eq!(restored_receipt.gas_used, receipt.gas_used);
            assert_eq!(restored_receipt.contract_address, receipt.contract_address);
            assert_eq!(restored_receipt.return_data, receipt.return_data);
        }
        assert_eq!(restored.height(), chain.height());
    }

    #[test]
    fn test_snapshot_uses_finalized_base_and_tracks_tip() {
        let validator = KeyPair::generate();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "snapshot-finalized-test".to_string(),
            chain_name: "snapshot-finalized-test".to_string(),
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
        })
        .unwrap();

        let block1 = chain.create_block(&validator).unwrap();
        chain.add_block(block1.clone()).unwrap();
        chain
            .block_tree
            .set_finalized(block1.hash.clone(), block1.header.height);
        chain.finality_tracker =
            FinalityTracker::with_finalized(block1.header.height, block1.hash.clone());

        let block2 = chain.create_block(&validator).unwrap();
        chain.add_block(block2.clone()).unwrap();

        let manifest = chain.create_snapshot().unwrap();
        assert_eq!(manifest.height, 1);
        assert_eq!(manifest.latest_hash, block1.hash);
        assert_eq!(manifest.tip_height, 2);
        assert_eq!(manifest.tip_hash, block2.hash);
    }

    #[test]
    fn test_snapshot_rejects_tampered_chunk_proof() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-snapshot-proof-test".to_string(),
            chain_name: "curs3d-snapshot-proof-test".to_string(),
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
        let mut chain =
            Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis)).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let manifest = chain.create_snapshot().unwrap();
        let mut chunks = chain.get_snapshot_chunks(manifest.height).unwrap();
        if let Some(first) = chunks.first_mut() {
            if let Some(proof_hash) = first.proof.first_mut() {
                proof_hash[0] ^= 0xFF;
            } else {
                first.proof.push(vec![0xAA; 32]);
            }
        }

        let mut restored = Blockchain::from_genesis(genesis).unwrap();
        let err = restored.apply_snapshot(&manifest, &chunks).unwrap_err();
        assert!(matches!(err, ChainError::SnapshotError(_)));
    }

    #[test]
    fn test_restart_restores_contracts_and_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-restart-test".to_string(),
            chain_name: "curs3d-restart-test".to_string(),
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

        let data_dir = dir.path().join("chain_db");
        let data_dir_str = data_dir.to_str().unwrap();

        let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 11)
        )"#
        .to_vec();
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

        let expected_contracts = chain.contracts.clone();
        let expected_receipts = chain.receipts.clone();
        let expected_height = chain.height();

        drop(chain);

        let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted.height(), expected_height);
        assert_eq!(restarted.contracts, expected_contracts);
        assert_eq!(restarted.receipts.len(), expected_receipts.len());
        for (tx_hash, receipt) in expected_receipts {
            let restored = restarted.receipts.get(&tx_hash).unwrap();
            assert_eq!(restored.success, receipt.success);
            assert_eq!(restored.gas_used, receipt.gas_used);
            assert_eq!(restored.contract_address, receipt.contract_address);
        }
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
