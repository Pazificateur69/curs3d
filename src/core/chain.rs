use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::consensus::{
    EpochSnapshot, EquivocationEvidence, FinalityTracker, FinalityVote, FinalizedBlock,
    ProofOfStake, allowed_backup_rank, slot_leader_at_rank,
};
use crate::core::block::{Block, EMPTY_STATE_ROOT_SEED};
use crate::core::blocktree::{BlockTree, BlockTreeError};
use crate::core::receipt::{IndexedLogEntry, IndexedReceipt, LogFilter, Receipt, ReceiptLocation};
use crate::core::state_proof::{AccountProof, StorageProof};
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::dilithium::KeyPair;
use crate::crypto::hash;
use crate::governance::GovernanceState;
use crate::storage::{Storage, StorageError};
use crate::token::TokenRegistry;
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
const MAX_PENDING_GAS_PER_ACCOUNT_MULTIPLIER: u64 = 2;
const MAX_PENDING_NONCE_GAP: u64 = 32;
const MIN_REPLACEMENT_FEE_BUMP_PCT: u64 = 10;
const MIN_REPLACEMENT_PRIORITY_BUMP_PCT: u64 = 25;
/// Hard cap on deployed contract bytecode (256 KB). The implicit gas-based
/// limit (block_gas_limit / GAS_PER_BYTE ≈ 625 KB) is loose; this cap gives a
/// clearer error and bounds long-term storage growth.
pub const MAX_CONTRACT_CODE_BYTES: usize = 256 * 1024;
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
    #[error(
        "wrong proposer: expected slot-leader at rank ≤ {allowed_rank}, got non-leader for height {height}"
    )]
    WrongProposer { height: u64, allowed_rank: u32 },
    #[error("block tree error: {0}")]
    BlockTree(#[from] BlockTreeError),
    #[error("reorg blocked by finality at height {0}")]
    ReorgBelowFinality(u64),
    #[error("invalid protocol version: expected {expected}, got {got}")]
    InvalidProtocolVersion { expected: u32, got: u32 },
    #[error("gas calculation overflowed")]
    GasOverflow,
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionEstimate {
    pub next_block_height: u64,
    pub base_fee_per_gas: u64,
    pub gas_used: u64,
    pub effective_gas_price: u64,
    pub priority_fee_paid: u64,
    pub base_fee_burned: u64,
    pub total_fee_charged: u64,
    pub gas_refunded: u64,
    pub max_total_fee: u64,
    pub would_replace_pending: bool,
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
    pub receipt_locations: HashMap<Vec<u8>, ReceiptLocation>,
    pub log_index: Vec<IndexedLogEntry>,
    pub token_registry: TokenRegistry,
    pub governance: GovernanceState,
    /// Tracks consecutive missed epochs per validator address (for inactivity penalties)
    pub validator_missed_epochs: HashMap<Vec<u8>, u64>,
    /// In-memory index from block hash to block height. Avoids O(n) scans for
    /// `block-by-hash` and `eth_getBlockByHash` lookups. Rebuilt on startup,
    /// kept in sync inside `add_block` / reorg paths.
    pub block_hash_to_height: HashMap<Vec<u8>, u64>,
    /// In-memory index from tx hash to (block_height, tx_index). Speeds up
    /// `tx/:hash`, `eth_getTransactionByHash` and any historical lookup.
    pub tx_hash_index: HashMap<Vec<u8>, (u64, usize)>,
    storage: Option<Storage>,
}

struct BlockExecution {
    accounts: HashMap<Vec<u8>, AccountState>,
    contracts: HashMap<Vec<u8>, ContractState>,
    receipts: HashMap<Vec<u8>, Receipt>,
    token_registry: TokenRegistry,
    governance: GovernanceState,
}

impl Default for Blockchain {
    fn default() -> Self {
        Self::new()
    }
}

impl Blockchain {
    #[allow(dead_code)]
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
            Self::build_epoch_snapshot_for_accounts(&genesis_config, 0, &accounts, &HashSet::new()),
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
            receipt_locations: HashMap::new(),
            log_index: Vec::new(),
            token_registry: TokenRegistry::new(),
            governance: GovernanceState::new(),
            validator_missed_epochs: HashMap::new(),
            block_hash_to_height: HashMap::new(),
            tx_hash_index: HashMap::new(),
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

            if let Some(expected_genesis) = genesis_config
                && &stored_genesis != expected_genesis
            {
                return Err(ChainError::GenesisMismatch);
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
            let loaded_accounts_for_weights: HashMap<Vec<u8>, AccountState> =
                storage.get_all_accounts_compat()?.into_iter().collect();

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
            let finalized_height: u64 = storage.get_meta(b"finalized_height")?.unwrap_or(0);
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

            let stored_token_registry = storage
                .get_meta::<TokenRegistry>(crate::storage::TOKEN_REGISTRY_KEY)?
                .unwrap_or_default();
            let stored_governance = storage
                .get_meta::<GovernanceState>(crate::storage::GOVERNANCE_STATE_KEY)?
                .unwrap_or_default();

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
                receipt_locations: HashMap::new(),
                log_index: Vec::new(),
                token_registry: stored_token_registry,
                governance: stored_governance,
                validator_missed_epochs: HashMap::new(),
                block_hash_to_height: HashMap::new(),
                tx_hash_index: HashMap::new(),
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

    pub fn next_base_fee_per_gas(&self, parent: &Block) -> u64 {
        let parent_base_fee = if parent.header.height == 0 {
            parent
                .header
                .base_fee_per_gas
                .max(self.initial_base_fee_per_gas)
        } else {
            parent.header.base_fee_per_gas
        };
        let target = self.target_block_gas_usage();
        // Transition: if parent had base_fee=0 (legacy), start charging from 1
        if parent_base_fee == 0 {
            return 1;
        }
        if parent.header.gas_used == target {
            return parent_base_fee;
        }

        let delta = parent.header.gas_used.abs_diff(target);
        let change = parent_base_fee
            .max(1)
            .saturating_mul(delta)
            .checked_div(target.saturating_mul(self.base_fee_change_denominator.max(1)))
            .unwrap_or(0)
            .max(1);

        if parent.header.gas_used > target {
            parent_base_fee.saturating_add(change)
        } else {
            parent_base_fee.saturating_sub(change).max(1)
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

    fn snapshot_for_accounts(
        &self,
        epoch: u64,
        accounts: &HashMap<Vec<u8>, AccountState>,
    ) -> EpochSnapshot {
        Self::build_epoch_snapshot_for_accounts(
            &self.genesis_config,
            epoch,
            accounts,
            &self.slashed_validators,
        )
    }

    fn active_validator_total_stake(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
        height: u64,
    ) -> u64 {
        ProofOfStake::with_slashed(self.minimum_stake, self.slashed_validators.clone(), height)
            .active_validators(accounts)
            .into_iter()
            .map(|validator| validator.stake)
            .sum()
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
    ///
    /// Genesis (height 0) is always version 1 — the genesis block was minted
    /// before any consensus rule existed. From height 1 onward the baseline
    /// is version 5: ML-DSA-87 (FIPS-204) replaces NIST round-3 Dilithium-L5
    /// as the post-quantum signature scheme, so signatures produced by the
    /// browser wallet (`sdk/wasm`) verify on the node byte-for-byte. v5 is
    /// otherwise compatible with v4 (EVM dispatch, slot-leader scheduling).
    /// Genesis configs may still declare explicit `upgrades`; those override
    /// the baseline at their specified heights, in declaration order, for
    /// chains that need to model historical version transitions.
    pub fn protocol_version_at_height(&self, height: u64) -> u32 {
        if self.genesis_config.upgrades.is_empty() {
            // Default: ML-DSA-87 + slot-leader + EVM dispatch apply uniformly.
            // We return v5 even at height 0 so the gossipsub topic name
            // (which is derived from protocol_version) is identical across
            // freshly-started nodes and nodes that have already synced a
            // few blocks.
            return 5;
        }
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
        let (snapshot_accounts, snapshot_contracts, snapshot_receipts, _, _) =
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
                let _ = storage.put_snapshot_chunk(snapshot_height, chunk, Some(chunks.len()));
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
            return storage
                .get_snapshot_chunks(height)
                .map_err(ChainError::from);
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
        let snapshot_state: crate::storage::SnapshotState =
            bincode::deserialize(&payload).map_err(|e| ChainError::SnapshotError(e.to_string()))?;

        let accounts: HashMap<Vec<u8>, AccountState> =
            snapshot_state.accounts.iter().cloned().collect();
        let contracts: HashMap<Vec<u8>, ContractState> =
            snapshot_state.contracts.iter().cloned().collect();
        let computed_root = Self::compute_state_root_full(&accounts, &contracts);
        if computed_root != manifest.state_root {
            return Err(ChainError::SnapshotError("state root mismatch".to_string()));
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

        // Defence against silent history rewrite: for every block height we
        // already have locally, the snapshot must agree on the hash. Otherwise
        // a peer could feed us a different chain history that shares the same
        // genesis (#5).
        for snapshot_block in &snapshot_state.blocks {
            let h = snapshot_block.header.height as usize;
            if let Some(local_block) = self.blocks.get(h)
                && local_block.hash != snapshot_block.hash
            {
                return Err(ChainError::SnapshotError(format!(
                    "snapshot block at height {} disagrees with local canonical block",
                    snapshot_block.header.height
                )));
            }
        }

        self.blocks = snapshot_state.blocks;
        self.accounts = snapshot_state.accounts.into_iter().collect();
        self.contracts = snapshot_state.contracts.into_iter().collect();
        self.receipts = snapshot_state.receipts.into_iter().collect();
        self.rebuild_receipt_indexes();
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
        block_tree.set_finalized(manifest.finalized_hash.clone(), manifest.finalized_height);
        self.block_tree = block_tree;
        self.finality_tracker = FinalityTracker::with_finalized(
            manifest.finalized_height,
            manifest.finalized_hash.clone(),
        );

        self.persist_full_state()?;
        tracing::info!(
            target: "audit",
            event = "snapshot_applied",
            height = manifest.height,
            finalized_height = manifest.finalized_height,
            tip_height = manifest.tip_height,
        );
        Ok(())
    }

    #[allow(dead_code)]
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

    pub fn get_receipt(&self, tx_hash: &[u8]) -> Option<IndexedReceipt> {
        let receipt = self.receipts.get(tx_hash)?.clone();
        let location = self.receipt_locations.get(tx_hash)?.clone();
        Some(IndexedReceipt {
            tx_hash: tx_hash.to_vec(),
            block_height: location.block_height,
            tx_index: location.tx_index,
            receipt,
        })
    }

    pub fn query_logs(&self, filter: &LogFilter) -> Vec<IndexedLogEntry> {
        let limit = filter.limit.unwrap_or(100).min(1000);
        self.log_index
            .iter()
            .filter(|entry| {
                filter
                    .contract
                    .as_ref()
                    .is_none_or(|contract| &entry.contract == contract)
                    && filter
                        .topic
                        .as_ref()
                        .is_none_or(|topic| entry.topics.iter().any(|candidate| candidate == topic))
                    && filter.topics.as_ref().is_none_or(|positional| {
                        positional.iter().enumerate().all(|(i, expected)| {
                            expected.as_ref().is_none_or(|wanted| {
                                entry.topics.get(i).is_some_and(|actual| actual == wanted)
                            })
                        })
                    })
                    && filter
                        .from_block
                        .is_none_or(|from_block| entry.block_height >= from_block)
                    && filter
                        .to_block
                        .is_none_or(|to_block| entry.block_height <= to_block)
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Return all transactions involving `address` (as sender or recipient).
    /// Walks the chain newest-first and stops at `limit` results.
    pub fn transactions_for_address(
        &self,
        address: &[u8],
        from_block: Option<u64>,
        to_block: Option<u64>,
        limit: usize,
    ) -> Vec<(u64, usize, Transaction)> {
        let limit = limit.min(1000);
        let mut out = Vec::new();
        for block in self.blocks.iter().rev() {
            if let Some(to) = to_block
                && block.header.height > to
            {
                continue;
            }
            if let Some(from) = from_block
                && block.header.height < from
            {
                break;
            }
            for (idx, tx) in block.transactions.iter().enumerate() {
                if tx.from == address || tx.to == address {
                    out.push((block.header.height, idx, tx.clone()));
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

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

        let pending_base_fee = self.next_base_fee_per_gas(self.latest_block());
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

    fn rebuild_receipt_indexes(&mut self) {
        self.receipt_locations.clear();
        self.log_index.clear();

        for block in &self.blocks {
            for (tx_index, tx) in block.transactions.iter().enumerate() {
                if tx.is_coinbase() {
                    continue;
                }
                let tx_hash = tx.hash();
                let Some(receipt) = self.receipts.get(&tx_hash) else {
                    continue;
                };
                self.receipt_locations.insert(
                    tx_hash.clone(),
                    ReceiptLocation {
                        block_height: block.header.height,
                        tx_index,
                    },
                );
                for (log_index, log) in receipt.logs.iter().enumerate() {
                    self.log_index.push(IndexedLogEntry {
                        block_height: block.header.height,
                        tx_index,
                        log_index,
                        tx_hash: tx_hash.clone(),
                        contract: log.contract.clone(),
                        topics: log.topics.clone(),
                        data: log.data.clone(),
                    });
                }
            }
        }
        self.log_index.sort_by(|a, b| {
            a.block_height
                .cmp(&b.block_height)
                .then_with(|| a.tx_index.cmp(&b.tx_index))
                .then_with(|| a.log_index.cmp(&b.log_index))
        });
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
        if tx.estimated_gas_for_admission() > self.block_gas_limit {
            return Err(ChainError::InvalidTransactionFormat(
                "transaction gas limit exceeds block gas limit",
            ));
        }
        if tx.kind == TransactionKind::DeployContract && tx.to.len() > MAX_CONTRACT_CODE_BYTES {
            return Err(ChainError::InvalidTransactionFormat(
                "deploy contract: wasm code exceeds 256 KB limit",
            ));
        }
        let pending_base_fee = self.next_base_fee_per_gas(self.latest_block());
        if tx.max_fee_per_gas() < pending_base_fee {
            return Err(ChainError::FeeTooLow);
        }
        if tx
            .priority_fee_per_gas(pending_base_fee)
            .unwrap_or_default()
            < self.minimum_priority_fee_per_gas(&tx, pending_base_fee)
        {
            return Err(ChainError::FeeTooLow);
        }
        if tx.total_fee_cap() < self.minimum_admission_fee(&tx, pending_base_fee) {
            return Err(ChainError::FeeTooLow);
        }
        let expected_nonce_floor = self.accounts.get(&tx.from).map(|a| a.nonce).unwrap_or(0);
        if tx.nonce < expected_nonce_floor {
            return Err(ChainError::InvalidTransactionFormat(
                "transaction nonce below account nonce (stale transaction)",
            ));
        }
        if tx.nonce > expected_nonce_floor.saturating_add(MAX_PENDING_NONCE_GAP) {
            return Err(ChainError::InvalidTransactionFormat(
                "transaction nonce gap too large for mempool admission",
            ));
        }

        let replacement_index = self
            .pending_transactions
            .iter()
            .position(|pending| pending.from == tx.from && pending.nonce == tx.nonce);

        if self.pending_transactions.len() >= MAX_PENDING_TRANSACTIONS
            && replacement_index.is_none()
        {
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
        let sender_pending_gas: u64 = self
            .pending_transactions
            .iter()
            .filter(|pending| pending.from == tx.from)
            .map(Transaction::estimated_gas_for_admission)
            .sum();
        let sender_pending_gas_budget = self
            .block_gas_limit
            .saturating_mul(MAX_PENDING_GAS_PER_ACCOUNT_MULTIPLIER);
        if replacement_index.is_none()
            && sender_pending_gas.saturating_add(tx.estimated_gas_for_admission())
                > sender_pending_gas_budget
        {
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
            let min_total_fee_cap = existing
                .total_fee_cap()
                .saturating_add(
                    existing
                        .total_fee_cap()
                        .saturating_mul(MIN_REPLACEMENT_FEE_BUMP_PCT)
                        / 100,
                )
                .max(existing.total_fee_cap().saturating_add(1));
            let existing_priority = existing
                .priority_fee_per_gas(pending_base_fee)
                .unwrap_or_default();
            let min_priority_fee = existing_priority
                .saturating_add(
                    existing_priority.saturating_mul(MIN_REPLACEMENT_PRIORITY_BUMP_PCT) / 100,
                )
                .max(existing_priority.saturating_add(1));
            let min_max_fee_per_gas = existing
                .max_fee_per_gas()
                .saturating_add(
                    existing
                        .max_fee_per_gas()
                        .saturating_mul(MIN_REPLACEMENT_FEE_BUMP_PCT)
                        / 100,
                )
                .max(existing.max_fee_per_gas().saturating_add(1));
            if tx.total_fee_cap() < min_total_fee_cap
                || tx.max_fee_per_gas() < min_max_fee_per_gas
                || tx
                    .priority_fee_per_gas(pending_base_fee)
                    .unwrap_or_default()
                    < min_priority_fee
            {
                return Err(ChainError::ReplacementFeeTooLow);
            }
        }

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
                return Err(ChainError::DuplicateTransaction);
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

        Self::apply_user_transaction(
            &mut projected_accounts,
            &mut projected_contracts,
            &mut projected_receipts,
            &mut projected_token_registry,
            &mut projected_governance,
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
        tracing::info!(
            target: "audit",
            event = "tx_accepted",
            tx_hash = %hex::encode(&protected_hash),
            sender = %hex::encode(&protected_from),
            nonce = protected_nonce,
            pending_count = self.pending_transactions.len(),
        );
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
        // Production-side leader check: derive the allowed backup rank from
        // wall-clock vs. parent timestamp, so a validator that's woken up
        // late (because the primary is offline) can still produce when
        // legitimately authorized as a backup.
        let now = chrono::Utc::now().timestamp();
        let snapshot_size = self
            .snapshot_for_height(&self.accounts, &self.epoch_snapshots, height)
            .map(|s| s.validators.len())
            .unwrap_or(0);
        let allowed_rank = allowed_backup_rank(prev_block.header.timestamp, now, snapshot_size);
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
        Self::apply_epoch_settlement_for_block(
            height,
            self.epoch_length,
            &self.epoch_snapshots,
            &self.blocks,
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
        Self::apply_epoch_settlement_for_block(
            block.header.height,
            self.epoch_length,
            &self.epoch_snapshots,
            &self.blocks,
            &mut self.accounts,
            &mut self.validator_missed_epochs,
        );
        let prev = self.latest_block();
        let execution = self.validate_block_against_state(
            &block,
            prev,
            &self.accounts,
            &self.contracts,
            &self.token_registry,
            &self.governance,
        )?;

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
        self.blocks.push(block.clone());
        self.block_hash_to_height
            .insert(block.hash.clone(), block.header.height);
        for (tx_index, tx) in block.transactions.iter().enumerate() {
            self.tx_hash_index
                .insert(tx.hash(), (block.header.height, tx_index));
        }
        self.rebuild_receipt_indexes();
        self.remove_block_transactions_from_mempool(&block);
        self.persist_full_state()?;
        tracing::info!(
            target: "audit",
            event = "block_added",
            height = block.header.height,
            tx_count = block.transactions.len(),
            hash = %hex::encode(&block.hash),
        );

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
            tracing::info!(
                target: "audit",
                event = "block_finalized",
                height = finalized.height,
                hash = %hex::encode(&finalized.hash),
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
        let penalty =
            pos.slash_with_evidence(&mut self.accounts, evidence, self.jail_duration_blocks)?;
        self.slashed_validators = pos.slashed_validators;

        // Persist
        if let Some(ref storage) = self.storage {
            let _ = storage.put_evidence(evidence);
            let address = hash::address_bytes_from_public_key(&evidence.validator_public_key);
            if let Some(state) = self.accounts.get(&address) {
                let _ = storage.put_account(&address, state);
            }
            let _ = storage.flush();
        }

        tracing::warn!(
            target: "audit",
            event = "validator_slashed",
            validator = %hex::encode(hash::address_bytes_from_public_key(&evidence.validator_public_key)),
            height = evidence.height,
            penalty = penalty,
        );

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

    #[allow(dead_code)]
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
            && replay.contracts == self.contracts
            && replay.token_registry == self.token_registry
    }

    /// Attempt to add a block that may fork from the current canonical chain.
    /// If it builds on the tip, behaves like add_block.
    /// If it forks, inserts into the block tree and potentially reorgs.
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
            storage.put_meta(
                crate::storage::SCHEMA_VERSION_KEY,
                &crate::storage::CURRENT_SCHEMA_VERSION,
            )?;
            storage.put_meta(crate::storage::TOKEN_REGISTRY_KEY, &self.token_registry)?;
            storage.put_meta(crate::storage::GOVERNANCE_STATE_KEY, &self.governance)?;
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

        let leaves = Self::state_leaf_hashes(accounts, contracts);
        hash::merkle_root(&leaves)
    }

    fn account_leaf_hash(address: &[u8], state: &AccountState) -> Vec<u8> {
        let encoded =
            bincode::serialize(&(address, state)).expect("failed to serialize account leaf");
        hash::sha3_hash(&encoded)
    }

    fn contract_storage_root(contract: &ContractState) -> Vec<u8> {
        let mut storage_entries: Vec<(&Vec<u8>, &Vec<u8>)> = contract.storage.iter().collect();
        storage_entries.sort_by_key(|(a, _)| *a);
        let leaves: Vec<Vec<u8>> = storage_entries
            .into_iter()
            .map(|(key, value)| {
                let encoded =
                    bincode::serialize(&(key, value)).expect("failed to serialize storage leaf");
                hash::sha3_hash(&encoded)
            })
            .collect();
        hash::merkle_root(&leaves)
    }

    fn contract_leaf_hash(address: &[u8], state: &ContractState) -> Vec<u8> {
        let storage_root = Self::contract_storage_root(state);
        let code_hash = if state.code_hash.is_empty() {
            hash::sha3_hash(&state.code)
        } else {
            state.code_hash.clone()
        };
        let encoded = bincode::serialize(&(address, code_hash, &state.owner, storage_root))
            .expect("failed to serialize contract leaf");
        hash::sha3_hash(&encoded)
    }

    fn state_leaf_hashes(
        accounts: &HashMap<Vec<u8>, AccountState>,
        contracts: &HashMap<Vec<u8>, ContractState>,
    ) -> Vec<Vec<u8>> {
        let mut leaves: Vec<Vec<u8>> = Vec::new();

        let mut account_entries: Vec<(&Vec<u8>, &AccountState)> = accounts.iter().collect();
        account_entries.sort_by_key(|(a, _)| *a);
        for (address, state) in account_entries {
            leaves.push(Self::account_leaf_hash(address, state));
        }

        let mut contract_entries: Vec<(&Vec<u8>, &ContractState)> = contracts.iter().collect();
        contract_entries.sort_by_key(|(a, _)| *a);
        for (address, state) in contract_entries {
            leaves.push(Self::contract_leaf_hash(address, state));
        }

        leaves
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

    pub fn get_account_proof(&self, address: &[u8]) -> Option<AccountProof> {
        let state = self.accounts.get(address)?.clone();
        let mut account_entries: Vec<(&Vec<u8>, &AccountState)> = self.accounts.iter().collect();
        account_entries.sort_by_key(|(a, _)| *a);
        let account_index = account_entries
            .iter()
            .position(|(entry_address, _)| entry_address.as_slice() == address)?;
        let leaf_hash = Self::account_leaf_hash(address, &state);
        let leaves = Self::state_leaf_hashes(&self.accounts, &self.contracts);

        Some(AccountProof {
            address: address.to_vec(),
            state,
            leaf_index: account_index,
            leaf_hash,
            proof: hash::merkle_proof(&leaves, account_index),
            state_root: Self::compute_state_root_full(&self.accounts, &self.contracts),
        })
    }

    #[allow(dead_code)]
    pub fn verify_account_proof(proof: &AccountProof) -> bool {
        let expected_leaf = Self::account_leaf_hash(&proof.address, &proof.state);
        expected_leaf == proof.leaf_hash
            && hash::verify_merkle_proof(
                &proof.leaf_hash,
                &proof.proof,
                proof.leaf_index,
                &proof.state_root,
            )
    }

    pub fn get_storage_proof(&self, contract_address: &[u8], key: &[u8]) -> Option<StorageProof> {
        let contract = self.contracts.get(contract_address)?;
        let value = contract.storage.get(key)?.clone();

        let mut account_entries: Vec<(&Vec<u8>, &AccountState)> = self.accounts.iter().collect();
        account_entries.sort_by_key(|(a, _)| *a);
        let account_count = account_entries.len();

        let mut contract_entries: Vec<(&Vec<u8>, &ContractState)> = self.contracts.iter().collect();
        contract_entries.sort_by_key(|(a, _)| *a);
        let contract_position = contract_entries
            .iter()
            .position(|(entry_address, _)| entry_address.as_slice() == contract_address)?;

        let mut storage_entries: Vec<(&Vec<u8>, &Vec<u8>)> = contract.storage.iter().collect();
        storage_entries.sort_by_key(|(a, _)| *a);
        let storage_position = storage_entries
            .iter()
            .position(|(entry_key, _)| entry_key.as_slice() == key)?;
        let storage_leaves: Vec<Vec<u8>> = storage_entries
            .iter()
            .map(|(entry_key, entry_value)| {
                let encoded = bincode::serialize(&(entry_key, entry_value))
                    .expect("failed to serialize storage proof leaf");
                hash::sha3_hash(&encoded)
            })
            .collect();
        let storage_leaf_hash = storage_leaves[storage_position].clone();
        let storage_root = hash::merkle_root(&storage_leaves);
        let contract_leaf_hash = Self::contract_leaf_hash(contract_address, contract);
        let state_leaves = Self::state_leaf_hashes(&self.accounts, &self.contracts);
        let code_hash = if contract.code_hash.is_empty() {
            hash::sha3_hash(&contract.code)
        } else {
            contract.code_hash.clone()
        };

        Some(StorageProof {
            contract_address: contract_address.to_vec(),
            contract_code_hash: code_hash,
            contract_owner: contract.owner.clone(),
            key: key.to_vec(),
            value,
            storage_leaf_index: storage_position,
            storage_leaf_hash,
            storage_proof: hash::merkle_proof(&storage_leaves, storage_position),
            storage_root,
            contract_leaf_index: account_count + contract_position,
            contract_leaf_hash,
            contract_proof: hash::merkle_proof(&state_leaves, account_count + contract_position),
            state_root: Self::compute_state_root_full(&self.accounts, &self.contracts),
        })
    }

    #[allow(dead_code)]
    pub fn verify_storage_proof(proof: &StorageProof) -> bool {
        let storage_leaf = {
            let encoded = bincode::serialize(&(&proof.key, &proof.value))
                .expect("failed to serialize storage verification leaf");
            hash::sha3_hash(&encoded)
        };
        if storage_leaf != proof.storage_leaf_hash {
            return false;
        }
        if !hash::verify_merkle_proof(
            &proof.storage_leaf_hash,
            &proof.storage_proof,
            proof.storage_leaf_index,
            &proof.storage_root,
        ) {
            return false;
        }
        let contract_leaf = {
            let encoded = bincode::serialize(&(
                &proof.contract_address,
                &proof.contract_code_hash,
                &proof.contract_owner,
                &proof.storage_root,
            ))
            .expect("failed to serialize contract verification leaf");
            hash::sha3_hash(&encoded)
        };
        contract_leaf == proof.contract_leaf_hash
            && hash::verify_merkle_proof(
                &proof.contract_leaf_hash,
                &proof.contract_proof,
                proof.contract_leaf_index,
                &proof.state_root,
            )
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

    /// Return the snapshot to use for slot-leader selection at the given height.
    ///
    /// Prefers a frozen snapshot at the matching epoch. Falls back to a freshly
    /// computed snapshot when we're crossing into a new epoch and the snapshot
    /// hasn't been persisted yet (boundary block in `add_block`). The genesis
    /// snapshot stored in `from_genesis` is built at `start_height = 0` and is
    /// therefore empty (genesis validators activate at height 1); for any
    /// post-genesis lookup with an empty cached snapshot we re-derive a fresh
    /// snapshot from the current accounts so the slot-leader function has a
    /// non-empty validator set.
    fn snapshot_for_height(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
        epoch_snapshots: &HashMap<u64, EpochSnapshot>,
        block_height: u64,
    ) -> Option<EpochSnapshot> {
        let epoch = block_height / self.epoch_length.max(1);
        if let Some(snapshot) = epoch_snapshots.get(&epoch)
            && !snapshot.validators.is_empty()
        {
            return Some(snapshot.clone());
        }
        // Cached but empty (genesis-epoch corner case) — fall through to
        // live derivation below.
        if block_height > 0 {
            // Build a fresh snapshot keyed on the *block height* so genesis
            // validators (active_from_height = 1) actually pass the filter.
            let pos = ProofOfStake::with_slashed(
                self.minimum_stake,
                self.slashed_validators.clone(),
                block_height,
            );
            let validators = pos.active_validators(accounts);
            if validators.is_empty() {
                return None;
            }
            let total_stake: u64 = validators.iter().map(|v| v.stake).sum();
            return Some(EpochSnapshot {
                epoch,
                start_height: block_height,
                validators,
                total_stake,
            });
        }
        None
    }

    fn ensure_validator_is_authorized_for_accounts_at_rank(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
        epoch_snapshots: &HashMap<u64, EpochSnapshot>,
        validator_public_key: &[u8],
        block_height: u64,
        prev_hash: &[u8],
        allowed_rank: u32,
    ) -> Result<(), ChainError> {
        let proposer_address = hash::address_bytes_from_public_key(validator_public_key);

        if let Some(snapshot) = self.snapshot_for_height(accounts, epoch_snapshots, block_height) {
            // Empty snapshot → pre-stake bootstrap, anyone with a public key may
            // propose. Same liberal default that the legacy path applied.
            if snapshot.validators.is_empty() {
                return Ok(());
            }
            for rank in 0..=allowed_rank {
                if let Some(addr) = slot_leader_at_rank(&snapshot, block_height, prev_hash, rank)
                    && addr == proposer_address
                {
                    return Ok(());
                }
            }
            return Err(ChainError::WrongProposer {
                height: block_height,
                allowed_rank,
            });
        }

        // No snapshot anywhere (very early bootstrap) — fall back to live POS,
        // which loops over current accounts. This branch only fires for chains
        // running without epoch snapshots yet, which since the slot-leader
        // hard-fork should be rare.
        let pos = ProofOfStake::with_slashed(
            self.minimum_stake,
            self.slashed_validators.clone(),
            block_height,
        );
        match pos.select_validator(accounts, block_height, prev_hash) {
            Some(expected) if expected.public_key == validator_public_key => Ok(()),
            Some(_) => Err(ChainError::WrongProposer {
                height: block_height,
                allowed_rank,
            }),
            None => Ok(()),
        }
    }

    /// Apply epoch settlement (rewards + inactivity penalties) when the block
    /// at `block_height` crosses an epoch boundary. The result mutates
    /// `accounts` in place and updates `validator_missed_epochs`.
    ///
    /// This is intentionally a free function over its inputs (no `&self`):
    /// `add_block` invokes it on `self.accounts` while the boot-time replay
    /// (`rebuild_canonical_state` / `replay_state_to_tip`) invokes it on a
    /// local accounts map. Keeping the body in one place is what guarantees
    /// the live state root and the recomputed-on-restart state root agree —
    /// the previous divergence caused the `state_root_mismatch` crash loop
    /// at every multiple of `epoch_length` past `2 * epoch_length`.
    ///
    /// The first epoch boundary (`prev_epoch == 0`) intentionally skips
    /// settlement for the genesis epoch, matching the historical guard in
    /// `add_block`.
    fn apply_epoch_settlement_for_block(
        block_height: u64,
        epoch_length: u64,
        epoch_snapshots: &HashMap<u64, EpochSnapshot>,
        blocks: &[Block],
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        validator_missed_epochs: &mut HashMap<Vec<u8>, u64>,
    ) {
        let epoch_len = epoch_length.max(1);
        let prev_epoch = block_height.saturating_sub(1) / epoch_len;
        let new_epoch = block_height / epoch_len;
        if new_epoch <= prev_epoch || prev_epoch == 0 {
            return;
        }
        let Some(snapshot) = epoch_snapshots.get(&prev_epoch) else {
            return;
        };

        let epoch_start = prev_epoch * epoch_len;
        let epoch_end = new_epoch * epoch_len;
        let mut block_producers: HashMap<Vec<u8>, u64> = HashMap::new();
        for h in epoch_start..epoch_end {
            if let Some(b) = blocks.get(h as usize) {
                let addr = hash::address_bytes_from_public_key(&b.header.validator_public_key);
                *block_producers.entry(addr).or_default() += 1;
            }
        }

        let settlement = crate::consensus::compute_epoch_settlement(
            snapshot,
            &block_producers,
            validator_missed_epochs,
        );
        crate::consensus::apply_epoch_settlement(accounts, &settlement);

        // Update missed_epochs tracker: reset producers, increment non-producers.
        for validator in &snapshot.validators {
            if block_producers.contains_key(&validator.address) {
                validator_missed_epochs.remove(&validator.address);
            } else {
                *validator_missed_epochs
                    .entry(validator.address.clone())
                    .or_default() += 1;
            }
        }

        if settlement.total_rewards_distributed > 0 || settlement.total_penalties_applied > 0 {
            tracing::info!(
                target: "audit",
                event = "epoch_settlement",
                epoch = prev_epoch,
                rewards = settlement.total_rewards_distributed,
                penalties = settlement.total_penalties_applied,
            );
        }
    }

    /// Public helper: return the slot leader at the given (height, prev_hash, rank)
    /// using the chain's frozen epoch snapshots. Used by the network production
    /// loop to gate `create_block` calls — only the elected validator should
    /// build a block at every 10 s slot, with backups taking over after
    /// `BACKUP_LEADER_TIMEOUT_SECS` of silence.
    pub fn slot_leader_address(
        &self,
        block_height: u64,
        prev_hash: &[u8],
        rank: u32,
    ) -> Option<Vec<u8>> {
        let snapshot =
            self.snapshot_for_height(&self.accounts, &self.epoch_snapshots, block_height)?;
        if snapshot.validators.is_empty() {
            return None;
        }
        slot_leader_at_rank(&snapshot, block_height, prev_hash, rank)
    }

    fn validate_block_against_state(
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
        let allowed_rank = allowed_backup_rank(
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

        let computed_state_root =
            Self::compute_state_root_full(&projected_accounts, &projected_contracts);
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
                let leaf = Self::account_leaf_hash(addr, state);
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
                let leaf = Self::contract_leaf_hash(addr, state);
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

    #[allow(clippy::type_complexity)]
    fn replay_state_to_tip(
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
            Self::apply_epoch_settlement_for_block(
                block.header.height,
                self.epoch_length,
                &self.epoch_snapshots,
                &self.blocks,
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
    fn replay_state_to_canonical_height(
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
        if tx.max_fee_per_gas() < base_fee_per_gas || tx.total_fee_cap() < required {
            return Err(ChainError::FeeTooLow);
        }
        Ok(())
    }

    fn priority_fee_for_transaction(tx: &Transaction, gas_used: u64, base_fee_per_gas: u64) -> u64 {
        tx.priority_fee_per_gas(base_fee_per_gas)
            .unwrap_or_default()
            .saturating_mul(gas_used)
    }

    fn apply_token_or_governance_tx(
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

    #[allow(clippy::too_many_arguments)]
    /// Apply an EVM-style `DeployEvmContract` tx by building a fresh state
    /// view from the chain accounts/contracts, running revm, then merging
    /// the delta back. Sender balance/nonce updates from revm are applied
    /// after the surrounding fee-handling block in `apply_user_transaction`
    /// has already debited the sender, so we re-apply revm's nonce
    /// faithfully (it's identical to ours except for the +1 we did).
    fn apply_evm_deploy(
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
        let outcome = crate::vm::evm::deploy(
            state,
            caller,
            &tx.data,
            tx.amount,
            tx.gas_limit,
            base_fee_per_gas.max(1),
            current_height,
            base_fee_per_gas,
            DEFAULT_BLOCK_GAS_LIMIT,
        )
        .map_err(|e| ChainError::InvalidTransactionFormat(Self::leak_evm_err(e)))?;
        Self::merge_evm_outcome(accounts, contracts, &outcome);
        Ok(outcome)
    }

    fn apply_evm_call(
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
            DEFAULT_BLOCK_GAS_LIMIT,
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

    #[allow(clippy::too_many_arguments)]
    fn apply_user_transaction(
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
                sender.pending_unstakes.push(PendingUnstake {
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
                a.nonce
                    .cmp(&b.nonce)
                    .then_with(|| b.max_fee_per_gas().cmp(&a.max_fee_per_gas()))
            } else {
                Self::compare_fee_priority(a, b, base_fee_per_gas)
                    .then_with(|| a.timestamp.cmp(&b.timestamp))
                    .then_with(|| b.max_fee_per_gas().cmp(&a.max_fee_per_gas()))
            }
        });
    }

    fn prune_pending_transactions(&mut self) {
        let cutoff = chrono::Utc::now().timestamp() - MAX_PENDING_TX_AGE_SECS;
        let base_fee_per_gas = self.next_base_fee_per_gas(self.latest_block());
        let usage = self.pending_gas_usage();
        let budget = self.pending_gas_budget().max(1);
        self.pending_transactions.retain(|pending| {
            pending.timestamp >= cutoff
                && pending.max_fee_per_gas() >= base_fee_per_gas
                && pending
                    .priority_fee_per_gas(base_fee_per_gas)
                    .unwrap_or_default()
                    >= Self::minimum_priority_fee_per_gas_for_usage(pending, usage, budget)
        });
        self.sort_pending_transactions();
    }

    fn compare_fee_priority(
        a: &Transaction,
        b: &Transaction,
        base_fee_per_gas: u64,
    ) -> std::cmp::Ordering {
        let a_gas = a.estimated_gas_for_admission().max(1) as u128;
        let b_gas = b.estimated_gas_for_admission().max(1) as u128;
        let a_fee = a.priority_fee_per_gas(base_fee_per_gas).unwrap_or_default() as u128;
        let b_fee = b.priority_fee_per_gas(base_fee_per_gas).unwrap_or_default() as u128;
        (b_fee.saturating_mul(a_gas))
            .cmp(&a_fee.saturating_mul(b_gas))
            .then_with(|| b.max_fee_per_gas().cmp(&a.max_fee_per_gas()))
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
        let required_base = tx.effective_gas_limit().saturating_mul(base_fee_per_gas);
        let required_priority = tx
            .effective_gas_limit()
            .saturating_mul(self.minimum_priority_fee_per_gas(tx, base_fee_per_gas));
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
            return required_base.saturating_add(required_priority);
        }
        let units = tx.estimated_gas_for_admission().saturating_add(99_999) / 100_000;
        required_base
            .saturating_add(required_priority)
            .saturating_add(units.max(1).saturating_mul(surcharge))
    }

    fn minimum_priority_fee_per_gas(&self, tx: &Transaction, _base_fee_per_gas: u64) -> u64 {
        let usage = self.pending_gas_usage();
        let budget = self.pending_gas_budget().max(1);
        Self::minimum_priority_fee_per_gas_for_usage(tx, usage, budget)
    }

    fn minimum_priority_fee_per_gas_for_usage(tx: &Transaction, usage: u64, budget: u64) -> u64 {
        let occupancy_pct = usage.saturating_mul(100) / budget;
        let congestion_floor: u64 = if occupancy_pct >= 95 {
            12
        } else if occupancy_pct >= 85 {
            8
        } else if occupancy_pct >= 70 {
            4
        } else if occupancy_pct >= 50 {
            2
        } else {
            0
        };
        let gas_units = tx.estimated_gas_for_admission().saturating_add(249_999) / 250_000;
        congestion_floor.saturating_mul(gas_units.max(1))
    }

    fn worst_pending_transaction_index(&self) -> Option<usize> {
        let base_fee_per_gas = self.next_base_fee_per_gas(self.latest_block());
        self.pending_transactions
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                if a.from == b.from {
                    b.nonce
                        .cmp(&a.nonce)
                        .then_with(|| a.max_fee_per_gas().cmp(&b.max_fee_per_gas()))
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
        self.pending_transactions
            .retain(|pending| !(pending.from == evicted.from && pending.nonce > evicted.nonce));
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
        for block in &blocks {
            self.block_hash_to_height
                .insert(block.hash.clone(), block.header.height);
            for (tx_index, tx) in block.transactions.iter().enumerate() {
                self.tx_hash_index
                    .insert(tx.hash(), (block.header.height, tx_index));
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
            Self::apply_epoch_settlement_for_block(
                block.header.height,
                self.epoch_length,
                &self.epoch_snapshots,
                &blocks,
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
            0,
            0,
        )
        .with_fee_caps(1_000, 100);
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        // Genesis base_fee=0 for backwards compat, transitions to >=1 on first block
        let initial_fee = chain.current_base_fee_per_gas();
        assert_eq!(initial_fee, 0); // Genesis starts at 0
        let block1 = chain.create_block(&validator).unwrap();
        assert!(block1.header.gas_used > chain.target_block_gas_usage());
        chain.add_block(block1).unwrap();

        let block2 = chain.create_block(&validator).unwrap();
        assert!(block2.header.base_fee_per_gas > initial_fee);
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
        // Sender paid: 1000 transfer + gas fees (base_fee >= 1)
        let sender_balance = chain.get_balance(&sender_address);
        assert!(sender_balance < DEFAULT_BLOCK_REWARD * 2 - 1000);
        assert!(sender_balance > DEFAULT_BLOCK_REWARD * 2 - 1000 - 100_000); // Reasonable fee range
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

        let mut stake_tx = Transaction::stake(
            chain.chain_id(),
            validator.public_key.clone(),
            10_000_000,
            5,
            0,
        );
        stake_tx.sign(&validator);
        chain.add_transaction(stake_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(chain.get_staked_balance(&address), 10_000_000);
        // Balance = 2 block rewards - staked - gas fees
        let balance = chain.get_balance(&address);
        assert!(balance < DEFAULT_BLOCK_REWARD * 2 - 10_000_000);
        assert!(balance > DEFAULT_BLOCK_REWARD * 2 - 10_000_000 - 100_000);
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
        let keypairs: Vec<KeyPair> = (0..9).map(|_| KeyPair::generate()).collect();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "mempool-pressure-test".to_string(),
            chain_name: "mempool-pressure-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            block_gas_limit: 100_000,
            allocations: keypairs
                .iter()
                .map(|kp| GenesisAllocation {
                    public_key: hex::encode(&kp.public_key),
                    balance: 10_000_000,
                    staked_balance: 0,
                })
                .collect(),
            ..Default::default()
        })
        .unwrap();

        let wasm_code =
            br#"(module (memory (export "memory") 1) (func (export "curs3d_call")))"#.to_vec();
        for kp in keypairs.iter().take(8) {
            let mut tx = Transaction::deploy_contract(
                chain.chain_id(),
                kp.public_key.clone(),
                wasm_code.clone(),
                100_000,
                0,
                0,
            )
            .with_fee_caps(20, 1);
            tx.sign(kp);
            chain.add_transaction(tx).unwrap();
        }

        let premium = keypairs.last().unwrap();
        let mut tx3 = Transaction::deploy_contract(
            chain.chain_id(),
            premium.public_key.clone(),
            wasm_code,
            100_000,
            0,
            0,
        )
        .with_fee_caps(20, 10);
        tx3.sign(premium);
        chain.add_transaction(tx3.clone()).unwrap();

        assert!(!chain.pending_transactions.is_empty());
        assert!(chain.pending_transactions.len() < 9);
        assert!(
            chain
                .pending_transactions
                .iter()
                .any(|pending| pending.hash() == tx3.hash())
        );
        assert!(chain.pending_gas_usage() <= chain.pending_gas_budget());
    }

    #[test]
    fn test_replacement_requires_priority_fee_bump() {
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "replacement-fee-test".to_string(),
            chain_name: "replacement-fee-test".to_string(),
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 50_000_000,
                staked_balance: 0,
            }],
            ..Default::default()
        })
        .unwrap();

        let recipient_address = hash::address_bytes_from_public_key(&recipient.public_key);
        let mut tx1 = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            recipient_address.clone(),
            1_000,
            0,
            0,
        )
        .with_fee_caps(10, 2);
        tx1.sign(&validator);
        chain.add_transaction(tx1).unwrap();

        let mut replacement = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            recipient_address,
            2_000,
            0,
            0,
        )
        .with_fee_caps(20, 2);
        replacement.sign(&validator);
        let err = chain.add_transaction(replacement).unwrap_err();
        assert!(matches!(err, ChainError::ReplacementFeeTooLow));
    }

    #[test]
    fn test_estimate_transaction_reports_fee_breakdown() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let mut tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            hash::address_bytes_from_public_key(&recipient.public_key),
            1_000,
            0,
            0,
        )
        .with_fee_caps(10, 2);
        tx.sign(&validator);

        let estimate = chain.estimate_transaction(&tx).unwrap();
        assert_eq!(estimate.next_block_height, chain.height() + 1);
        assert!(estimate.gas_used > 0);
        assert!(estimate.total_fee_charged > 0);
        assert_eq!(
            estimate.priority_fee_paid + estimate.base_fee_burned + estimate.gas_refunded,
            estimate.max_total_fee
        );
    }

    #[test]
    fn test_rejects_excessive_pending_nonce_gap() {
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "nonce-gap-test".to_string(),
            chain_name: "nonce-gap-test".to_string(),
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 50_000_000,
                staked_balance: 0,
            }],
            ..Default::default()
        })
        .unwrap();

        let mut tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            hash::address_bytes_from_public_key(&recipient.public_key),
            1_000,
            0,
            MAX_PENDING_NONCE_GAP + 1,
        )
        .with_fee_caps(10, 2);
        tx.sign(&validator);
        let err = chain.add_transaction(tx).unwrap_err();
        assert!(matches!(err, ChainError::InvalidTransactionFormat(_)));
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
        let mut stake_tx = Transaction::stake(
            chain.chain_id(),
            validator.public_key.clone(),
            10_000_000,
            5,
            0,
        );
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
        // Balance includes block reward but minus gas fees for unstake tx
        assert!(
            balance_after_unstake_block < balance_after_stake + DEFAULT_BLOCK_REWARD - 5_000_000
        );
        assert!(
            balance_after_unstake_block
                > balance_after_stake + DEFAULT_BLOCK_REWARD - 5_000_000 - 100_000
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
        block.signature = Some(validator.sign(&Block::signable_block_hash(&block.hash)));

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

        let mut fork = chain.create_block(&validator).unwrap();
        fork.header.height = 1;
        fork.header.prev_hash = chain.blocks[0].hash.clone();
        fork.header.state_root = vec![7; 32];
        fork.hash = Block::compute_hash(&fork.header);
        fork.signature = Some(validator.sign(&Block::signable_block_hash(&fork.hash)));

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
            0,
            0,
        )
        .with_fee_caps(20, 2);
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
            0,
            0,
        )
        .with_fee_caps(20, 2);
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
            0,
            1,
        )
        .with_fee_caps(20, 3);
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
        assert!(call_receipt.effective_gas_price >= chain.current_base_fee_per_gas());
        assert!(call_receipt.gas_refunded > 0);
        assert_eq!(
            call_receipt.priority_fee_paid
                + call_receipt.base_fee_burned
                + call_receipt.gas_refunded,
            20 * 1_000_000
        );
    }

    /// End-to-end: deploy a real SDK-compiled wasm contract through the chain
    /// (not via Vm directly), call it twice, and verify the receipt + state +
    /// indexes (block_hash_to_height, tx_hash_index, log_index) all stay
    /// consistent. Skipped if the .wasm hasn't been built yet so CI without the
    /// wasm32 target keeps passing.
    #[test]
    fn test_sdk_counter_via_chain_integration() {
        const COUNTER_WASM: &[u8] = include_bytes!(
            "../../sdk/rust/examples/counter/target/wasm32-unknown-unknown/release/counter_contract.wasm"
        );
        if COUNTER_WASM.len() < 16 {
            return;
        }

        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        // Mine several blocks to give the validator enough liquid balance to pay
        // for the deploy + call gas budgets.
        for _ in 0..6 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        // Deploy the SDK contract via a real DeployContract tx
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            COUNTER_WASM.to_vec(),
            5_000_000,
            0,
            0,
        )
        .with_fee_caps(50, 5);
        deploy_tx.sign(&validator);
        let deploy_hash = deploy_tx.hash();
        chain.add_transaction(deploy_tx).unwrap();
        let block = chain.create_block(&validator).unwrap();
        let deploy_block_hash = block.hash.clone();
        let deploy_block_height = block.header.height;
        chain.add_block(block).unwrap();

        // Index integrity: deploy block + tx are both reachable in O(1)
        assert_eq!(
            chain.block_hash_to_height.get(&deploy_block_hash).copied(),
            Some(deploy_block_height)
        );
        assert_eq!(
            chain.tx_hash_index.get(&deploy_hash).map(|(h, _)| *h),
            Some(deploy_block_height)
        );

        let contract_address = chain
            .receipts
            .values()
            .find(|r| r.contract_address.is_some())
            .expect("deploy receipt missing")
            .contract_address
            .clone()
            .unwrap();

        // First call: counter goes 0 → 1
        let mut call1 = Transaction::call_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            contract_address.clone(),
            Vec::new(),
            0,
            2_000_000,
            0,
            1,
        )
        .with_fee_caps(50, 5);
        call1.sign(&validator);
        let call1_hash = call1.hash();
        chain.add_transaction(call1).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let receipt1 = chain
            .get_receipt(&call1_hash)
            .expect("call1 receipt missing");
        assert!(receipt1.receipt.success);
        assert_eq!(receipt1.receipt.logs.len(), 1);
        assert_eq!(receipt1.receipt.logs[0].topics[0], b"tick");
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&receipt1.receipt.logs[0].data[..8]);
        assert_eq!(u64::from_le_bytes(buf), 1);

        // Second call: counter goes 1 → 2 (proves storage persisted across blocks)
        let mut call2 = Transaction::call_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            contract_address.clone(),
            Vec::new(),
            0,
            2_000_000,
            0,
            2,
        )
        .with_fee_caps(50, 5);
        call2.sign(&validator);
        let call2_hash = call2.hash();
        chain.add_transaction(call2).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let receipt2 = chain
            .get_receipt(&call2_hash)
            .expect("call2 receipt missing");
        let mut buf2 = [0u8; 8];
        buf2.copy_from_slice(&receipt2.receipt.logs[0].data[..8]);
        assert_eq!(u64::from_le_bytes(buf2), 2);

        // Log index gets two `tick` entries on the same contract
        let logs = chain.query_logs(&LogFilter {
            contract: Some(contract_address),
            topic: Some(b"tick".to_vec()),
            topics: None,
            from_block: None,
            to_block: None,
            limit: Some(10),
        });
        assert_eq!(logs.len(), 2);
    }

    #[test]
    fn test_deploy_contract_rejects_oversize_wasm() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        // Mine a block to fund the validator
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // 257 KB of bytes prefixed with the wasm magic so the size check fires
        // before any wasm validation logic.
        let mut oversize = vec![0u8; MAX_CONTRACT_CODE_BYTES + 1024];
        oversize[..4].copy_from_slice(b"\0asm");
        let mut tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            oversize,
            5_000_000,
            0,
            0,
        )
        .with_fee_caps(50, 5);
        tx.sign(&validator);

        let err = chain.add_transaction(tx).unwrap_err();
        match err {
            ChainError::InvalidTransactionFormat(msg) => {
                assert!(
                    msg.contains("256 KB"),
                    "expected size-limit error, got: {}",
                    msg
                );
            }
            other => panic!("expected InvalidTransactionFormat, got {:?}", other),
        }
    }

    #[test]
    fn test_block_hash_index_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();

        let allocations = vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 0,
        }];
        let genesis_config = GenesisConfig {
            allocations,
            ..GenesisConfig::default()
        };

        let mut block_hashes = Vec::new();
        {
            let mut chain =
                Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis_config))
                    .unwrap();
            for _ in 0..3 {
                let block = chain.create_block(&validator).unwrap();
                block_hashes.push(block.hash.clone());
                chain.add_block(block).unwrap();
            }
            // Index populated in-memory after add_block
            for (i, h) in block_hashes.iter().enumerate() {
                assert_eq!(
                    chain.block_hash_to_height.get(h).copied(),
                    Some((i + 1) as u64)
                );
            }
        }
        // Reopen from disk: rebuild_canonical_state must repopulate the index
        let chain =
            Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis_config)).unwrap();
        for (i, h) in block_hashes.iter().enumerate() {
            assert_eq!(
                chain.block_hash_to_height.get(h).copied(),
                Some((i + 1) as u64),
                "block hash index must survive restart"
            );
        }
    }

    #[test]
    fn test_account_and_storage_proofs_roundtrip() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let wasm_code = br#"(module
            (import "curs3d" "storage_write_bytes" (func $storage_write_bytes (param i32 i32 i32 i32) (result i32)))
            (import "curs3d" "storage_read" (func $storage_read (param i32 i32 i32 i32) (result i32)))
            (import "curs3d" "emit_log_bytes" (func $emit_log_bytes (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "key1")
            (data (i32.const 16) "val1")
            (data (i32.const 32) "topic")
            (func (export "curs3d_call") (result i32)
                i32.const 0
                i32.const 4
                i32.const 16
                i32.const 4
                call $storage_write_bytes
                drop
                i32.const 0
                i32.const 4
                i32.const 64
                i32.const 16
                call $storage_read
                drop
                i32.const 32
                i32.const 5
                i32.const 64
                i32.const 4
                call $emit_log_bytes
                drop
                i32.const 1)
        )"#
        .to_vec();

        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            1_000_000,
            0,
            0,
        )
        .with_fee_caps(20, 2);
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let contract_address = chain
            .receipts
            .values()
            .find_map(|receipt| receipt.contract_address.clone())
            .unwrap();

        let mut call_tx = Transaction::call_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            contract_address.clone(),
            Vec::new(),
            0,
            1_000_000,
            0,
            1,
        )
        .with_fee_caps(20, 2);
        let call_tx_hash = call_tx.hash();
        call_tx.sign(&validator);
        chain.add_transaction(call_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let validator_address = hash::address_bytes_from_public_key(&validator.public_key);
        let account_proof = chain.get_account_proof(&validator_address).unwrap();
        assert!(Blockchain::verify_account_proof(&account_proof));

        let storage_proof = chain.get_storage_proof(&contract_address, b"key1").unwrap();
        assert_eq!(storage_proof.value, b"val1".to_vec());
        assert!(Blockchain::verify_storage_proof(&storage_proof));

        let indexed_receipt = chain.get_receipt(&call_tx_hash).unwrap();
        assert_eq!(indexed_receipt.block_height, 3);
        assert_eq!(indexed_receipt.receipt.logs.len(), 1);

        let logs = chain.query_logs(&LogFilter {
            contract: Some(contract_address.clone()),
            topic: Some(b"topic".to_vec()),
            topics: None,
            from_block: Some(0),
            to_block: Some(chain.height()),
            limit: Some(10),
        });
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].tx_hash, call_tx_hash);
        assert_eq!(logs[0].data, b"val1".to_vec());

        // Positional topics filter (eth-style): topic at index 0 must equal "topic"
        let logs_positional = chain.query_logs(&LogFilter {
            contract: Some(contract_address.clone()),
            topic: None,
            topics: Some(vec![Some(b"topic".to_vec())]),
            from_block: None,
            to_block: None,
            limit: Some(10),
        });
        assert_eq!(logs_positional.len(), 1);

        // Wildcard at position 0 matches everything
        let logs_wildcard = chain.query_logs(&LogFilter {
            contract: Some(contract_address.clone()),
            topic: None,
            topics: Some(vec![None]),
            from_block: None,
            to_block: None,
            limit: Some(10),
        });
        assert_eq!(logs_wildcard.len(), 1);

        // Wrong topic at position 0 matches nothing
        let logs_no_match = chain.query_logs(&LogFilter {
            contract: Some(contract_address),
            topic: None,
            topics: Some(vec![Some(b"other".to_vec())]),
            from_block: None,
            to_block: None,
            limit: Some(10),
        });
        assert_eq!(logs_no_match.len(), 0);
    }

    #[test]
    fn test_transactions_for_address_returns_sent_and_received() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        // Block 1: validator gets the coinbase reward
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Validator sends a transfer to a fresh address
        let recipient = vec![9u8; hash::ADDRESS_LEN];
        let mut transfer_tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            recipient.clone(),
            1_000,
            100,
            0,
        );
        transfer_tx.sign(&validator);
        let transfer_hash = transfer_tx.hash();
        chain.add_transaction(transfer_tx).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let validator_addr = hash::address_bytes_from_public_key(&validator.public_key);
        let validator_txs = chain.transactions_for_address(&validator_addr, None, None, 50);
        // Validator was sender of the transfer + recipient of coinbase blocks
        assert!(
            validator_txs
                .iter()
                .any(|(_, _, tx)| tx.hash() == transfer_hash),
            "should include the transfer the validator sent"
        );

        let recipient_txs = chain.transactions_for_address(&recipient, None, None, 50);
        assert_eq!(recipient_txs.len(), 1);
        assert_eq!(recipient_txs[0].2.hash(), transfer_hash);

        // Limit clamps the result
        let limited = chain.transactions_for_address(&validator_addr, None, None, 1);
        assert_eq!(limited.len(), 1);
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
        let selected =
            ProofOfStake::select_validator_from_snapshot(snapshot, 5, chain.latest_hash());
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
        let mut chain =
            Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis)).unwrap();

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

    /// Regression test for #5: a peer that ships a snapshot whose blocks
    /// disagree with our locally stored canonical chain at any height must
    /// be rejected, not silently overwrite our history.
    ///
    /// The test deliberately puts the local node at a SHORTER height than
    /// the snapshot tip so that the existing `manifest.height` check
    /// (which only looks at the tip) cannot fire — only the new per-height
    /// loop should reject the snapshot.
    #[test]
    fn test_snapshot_rejects_divergent_block_hash_at_any_height() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-snapshot-divergent-test".to_string(),
            chain_name: "curs3d-snapshot-divergent-test".to_string(),
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

        // chain_a = our local node, only one block past genesis.
        let mut chain_a =
            Blockchain::with_storage(dir_a.path().to_str().unwrap(), Some(&genesis)).unwrap();
        let block = chain_a.create_block(&validator).unwrap();
        chain_a.add_block(block).unwrap();
        let local_block_1_hash = chain_a.blocks[1].hash.clone();

        // chain_b = remote peer with a divergent history, several blocks ahead.
        // To force divergence at height 1 (block creation is otherwise deterministic
        // when the validator and parent are identical), wait one second so the
        // block timestamp differs.
        std::thread::sleep(std::time::Duration::from_secs(1));
        let mut chain_b =
            Blockchain::with_storage(dir_b.path().to_str().unwrap(), Some(&genesis)).unwrap();
        for _ in 0..5 {
            let block = chain_b.create_block(&validator).unwrap();
            chain_b.add_block(block).unwrap();
        }
        // Sanity: chain_b's block at height 1 must disagree with chain_a's.
        assert_ne!(chain_b.blocks[1].hash, local_block_1_hash);
        // chain_a is shorter than chain_b's snapshot tip, so the legacy
        // tip-only check cannot fire here.
        assert!(chain_a.height() < chain_b.height());

        let manifest_b = chain_b.create_snapshot().unwrap();
        let chunks_b = chain_b.get_snapshot_chunks(manifest_b.height).unwrap();
        let err = chain_a.apply_snapshot(&manifest_b, &chunks_b).unwrap_err();
        assert!(matches!(err, ChainError::SnapshotError(_)));
        // Local chain must be untouched.
        assert_eq!(chain_a.blocks[1].hash, local_block_1_hash);
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

    /// Regression test for #2: build a chain, drop it, reload from disk, and
    /// verify the recomputed state root matches every persisted block header.
    /// If a non-determinism creeps into state-root computation (HashMap
    /// iteration order, leaky governance state, etc.) this will fail with
    /// `InvalidStateRoot` on the second `with_storage`.
    #[test]
    fn test_state_root_deterministic_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-state-root-restart".to_string(),
            chain_name: "curs3d-state-root-restart".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&validator.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
                GenesisAllocation {
                    public_key: hex::encode(&recipient.public_key),
                    balance: 0,
                    staked_balance: 0,
                },
            ],
            ..Default::default()
        };

        let data_dir = dir.path().join("chain_db");
        let data_dir_str = data_dir.to_str().unwrap();

        let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        // Build a few blocks with mixed activity (transfer + stake unwinding +
        // contract deploy) so the state graph has enough surface area to
        // expose non-determinism if any creeps in.
        let recipient_addr = hash::address_bytes_from_public_key(&recipient.public_key);
        for i in 0..5u64 {
            let mut tx = Transaction::new(
                chain.chain_id(),
                validator.public_key.clone(),
                recipient_addr.clone(),
                100,
                10,
                i,
            );
            tx.sign(&validator);
            chain.add_transaction(tx).unwrap();
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }
        let expected_height = chain.height();
        let expected_state_roots: Vec<Vec<u8>> = chain
            .blocks
            .iter()
            .map(|b| b.header.state_root.clone())
            .collect();

        drop(chain);

        // Reloading must succeed; rebuild_canonical_state would otherwise
        // raise InvalidStateRoot during replay.
        let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted.height(), expected_height);
        for (h, expected_root) in expected_state_roots.iter().enumerate() {
            assert_eq!(
                &restarted.blocks[h].header.state_root, expected_root,
                "state_root for block {} diverged after restart",
                h
            );
        }
    }

    #[test]
    fn test_restart_restores_token_registry_and_governance() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-governance-token-test".to_string(),
            chain_name: "curs3d-governance-token-test".to_string(),
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

        let mut deploy_token_tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            Vec::new(),
            0,
            100,
            0,
        );
        deploy_token_tx.kind = TransactionKind::DeployToken;
        deploy_token_tx.data = serde_json::to_vec(&crate::token::DeployTokenParams {
            name: "Persisted Token".to_string(),
            symbol: "PST".to_string(),
            decimals: 6,
            total_supply: 1_000_000,
        })
        .unwrap();
        deploy_token_tx.sign(&validator);
        chain.add_transaction(deploy_token_tx).unwrap();

        let mut proposal_tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            Vec::new(),
            0,
            100,
            1,
        );
        proposal_tx.kind = TransactionKind::SubmitProposal;
        proposal_tx.data = serde_json::to_vec(&crate::governance::SubmitProposalParams {
            kind: crate::governance::ProposalKind::ParameterChange {
                parameter: "block_gas_limit".to_string(),
                new_value: 20_000_000,
            },
        })
        .unwrap();
        proposal_tx.sign(&validator);
        chain.add_transaction(proposal_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let expected_registry = chain.token_registry.clone();
        let expected_governance_ids: Vec<Vec<u8>> = chain
            .governance
            .list_proposals()
            .iter()
            .map(|p| p.id.clone())
            .collect();

        assert_eq!(expected_registry.tokens.len(), 1);
        assert_eq!(expected_governance_ids.len(), 1);

        drop(chain);

        let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted.token_registry, expected_registry);
        let restarted_ids: Vec<Vec<u8>> = restarted
            .governance
            .list_proposals()
            .iter()
            .map(|p| p.id.clone())
            .collect();
        assert_eq!(restarted_ids, expected_governance_ids);
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
        block.signature = Some(validator.sign(&Block::signable_block_hash(&block.hash)));

        let err = chain.add_block(block).unwrap_err();
        assert!(matches!(err, ChainError::InvalidProtocolVersion { .. }));
    }

    #[test]
    fn test_block_rejected_if_wrong_proposer() {
        // Two validators in genesis with equal stake. After the legitimate
        // leader produces block 1 (advancing the parent timestamp to ~now),
        // the non-leader at height 2 must be rejected with WrongProposer
        // because no backup-leader timeout has elapsed yet.
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-wrong-proposer-test".to_string(),
            chain_name: "curs3d-wrong-proposer-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&kp_a.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
                GenesisAllocation {
                    public_key: hex::encode(&kp_b.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
            ],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        // Mine block 1 with whichever validator is elected at height 1. This
        // refreshes the parent timestamp to ~now so the rank-0 window is
        // active for height 2.
        let leader_h1 = chain
            .slot_leader_address(1, chain.latest_hash(), 0)
            .expect("slot leader at height 1");
        let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
        let real_kp_h1 = if leader_h1 == addr_a { &kp_a } else { &kp_b };
        let block1 = chain.create_block(real_kp_h1).unwrap();
        chain.add_block(block1).unwrap();

        // Now identify the rank-0 leader at height 2 and pick the *other*
        // keypair as the imposter.
        let leader_h2 = chain
            .slot_leader_address(2, chain.latest_hash(), 0)
            .expect("slot leader at height 2");
        let imposter_kp = if leader_h2 == addr_a { &kp_b } else { &kp_a };

        // The non-leader's create_block fails fast with WrongProposer.
        let err = chain
            .create_block(imposter_kp)
            .expect_err("non-leader should not be allowed to produce");
        assert!(
            matches!(err, ChainError::WrongProposer { height: 2, .. }),
            "expected WrongProposer at height 2, got {:?}",
            err
        );
    }

    #[test]
    fn test_two_validators_alternate() {
        // Sample a long run of slot-leader picks across two equal-stake
        // validators. Verify a single block per height (no fork) and a roughly
        // 50/50 split. With only `slot_leader` as the gate, both validators
        // converge to the same producer per height.
        use crate::consensus::slot_leader;
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
        let addr_b = hash::address_bytes_from_public_key(&kp_b.public_key);
        let genesis = GenesisConfig {
            chain_id: "curs3d-two-validator-test".to_string(),
            chain_name: "curs3d-two-validator-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&kp_a.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
                GenesisAllocation {
                    public_key: hex::encode(&kp_b.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
            ],
            ..Default::default()
        };
        let chain = Blockchain::from_genesis(genesis).unwrap();
        // Walk a synthetic 30-slot ledger. Each height: only the elected
        // leader produces — exactly one block per height — and the other
        // validator stays quiet. 30 samples make a degenerate 30/0 split
        // astronomically unlikely (P < 1e-9) for an honest hash function.
        // We go through `slot_leader_address` so the per-height live-snapshot
        // fallback applies (the genesis-epoch cached snapshot is empty by
        // construction; see `snapshot_for_height`).
        let _ = slot_leader; // imported for symmetry with the bare-snapshot API
        let mut leaders: Vec<Vec<u8>> = Vec::new();
        let parent = chain.latest_hash().to_vec();
        for h in 1..=30 {
            let leader = chain.slot_leader_address(h, &parent, 0).unwrap();
            leaders.push(leader);
        }

        // Single producer per height (vacuously true here, but proves the
        // function returns a unique address per slot).
        for leader in &leaders {
            assert!(*leader == addr_a || *leader == addr_b);
        }

        let count_a = leaders.iter().filter(|a| **a == addr_a).count();
        let count_b = leaders.iter().filter(|a| **a == addr_b).count();
        assert_eq!(count_a + count_b, 30);
        // 50/50 in expectation; allow any non-degenerate split.
        assert!(count_a > 0, "validator A never produced");
        assert!(count_b > 0, "validator B never produced");
        // Ratio within ±50% of expected 15 (very loose to absorb sampling).
        assert!(
            (5..=25).contains(&count_a),
            "split too skewed: a={}, b={}",
            count_a,
            count_b
        );
    }

    /// Regression test for the `state_root_mismatch` bug at the second epoch
    /// boundary on multi-validator chains.
    ///
    /// Symptom: a node that has lived past `h = 2 * epoch_length` (the first
    /// height where epoch settlement actually distributes rewards — the
    /// `prev_epoch > 0` guard in `add_block` skips settlement at the very
    /// first epoch boundary `h = epoch_length`) crashes on restart with
    /// `Failed to initialize blockchain storage: invalid state root`.
    ///
    /// Root cause: `add_block` mutates `self.accounts` via
    /// `consensus::apply_epoch_settlement` *before* calling
    /// `validate_block_against_state` — the live state root therefore reflects
    /// post-settlement balances. But the boot path
    /// (`with_storage` -> `rebuild_canonical_state`) replays each block via
    /// `validate_block_against_state` *without* applying the same settlement
    /// step on the parent accounts handed in. The recomputed state root for
    /// the boundary block is therefore strictly less (rewards never granted)
    /// and the chain refuses to load.
    ///
    /// This test reproduces the failure with a 2-validator genesis (similar
    /// to the live testnet) and a small `epoch_length` of 4, mining past
    /// `h = 2 * epoch_length = 8` so that the *second* epoch boundary
    /// settles non-zero rewards. With the bug, the second `with_storage`
    /// returns `ChainError::InvalidStateRoot`. With the fix it succeeds.
    #[test]
    fn test_restart_across_epoch_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
        // Use a tiny epoch length so we cross the *second* boundary quickly.
        // EPOCH_REWARD_RATE_PER_CUR is 100 microtokens per CUR per block, so
        // we need staked >= 1 CUR (= 1_000_000 microtokens) for rewards to
        // be non-zero — the bug is otherwise masked by the saturating
        // arithmetic.
        let epoch_length: u64 = 4;
        let stake = 1_000_000_000u64; // 1000 CUR — comfortably above minimum_stake.
        let genesis = GenesisConfig {
            chain_id: "curs3d-restart-epoch-test".to_string(),
            chain_name: "curs3d-restart-epoch-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&kp_a.public_key),
                    balance: 1_000_000_000,
                    staked_balance: stake,
                },
                GenesisAllocation {
                    public_key: hex::encode(&kp_b.public_key),
                    balance: 1_000_000_000,
                    staked_balance: stake,
                },
            ],
            ..Default::default()
        };
        let data_dir = dir.path().join("chain_db");
        let data_dir_str = data_dir.to_str().unwrap();

        // Mine across the first *two* epoch boundaries so the bug actually
        // fires. Settlement at h=epoch_length is skipped (prev_epoch == 0),
        // settlement at h=2*epoch_length runs and grants rewards. After that
        // any restart re-validates block 2*epoch_length and trips the bug.
        let target_height = 2 * epoch_length + 1;
        let expected_state_roots: Vec<Vec<u8>>;
        let expected_height: u64;
        {
            let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
            for h in 1..=target_height {
                let leader = chain
                    .slot_leader_address(h, chain.latest_hash(), 0)
                    .expect("slot leader exists");
                let kp = if leader == addr_a { &kp_a } else { &kp_b };
                let block = chain.create_block(kp).unwrap();
                chain.add_block(block).unwrap();
            }
            expected_height = chain.height();
            expected_state_roots = chain
                .blocks
                .iter()
                .map(|b| b.header.state_root.clone())
                .collect();
            assert_eq!(expected_height, target_height);
        }

        // Reload — this is what fails with `invalid state root` in the wild.
        let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted.height(), expected_height);
        for (h, expected_root) in expected_state_roots.iter().enumerate() {
            assert_eq!(
                &restarted.blocks[h].header.state_root, expected_root,
                "state_root for block {} diverged after restart",
                h
            );
        }
        // Open a third time as paranoia: ensure replay is idempotent.
        drop(restarted);
        let restarted_again = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted_again.height(), expected_height);
    }
}
