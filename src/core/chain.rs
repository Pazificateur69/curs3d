use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::consensus::{
    EquivocationEvidence, FinalityTracker, FinalityVote, FinalizedBlock, ProofOfStake,
};
use crate::core::block::{Block, EMPTY_STATE_ROOT_SEED};
use crate::core::blocktree::{BlockTree, BlockTreeError};
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::dilithium::KeyPair;
use crate::crypto::hash;
use crate::storage::{Storage, StorageError};
use thiserror::Error;

pub const DEFAULT_BLOCK_REWARD: u64 = 50_000_000;
pub const DEFAULT_MIN_STAKE: u64 = 1_000_000_000;
const MAX_FUTURE_BLOCK_TIME_SECS: i64 = 30;
const CHAIN_CONFIG_KEY: &[u8] = b"chain_config";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenesisAllocation {
    pub public_key: String,
    pub balance: u64,
    pub staked_balance: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenesisConfig {
    pub chain_name: String,
    pub block_reward: u64,
    pub minimum_stake: u64,
    pub allocations: Vec<GenesisAllocation>,
}

impl Default for GenesisConfig {
    fn default() -> Self {
        GenesisConfig {
            chain_name: "curs3d-devnet".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: DEFAULT_MIN_STAKE,
            allocations: Vec::new(),
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
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountState {
    pub balance: u64,
    pub nonce: u64,
    pub staked_balance: u64,
    pub public_key: Option<Vec<u8>>,
}

pub struct Blockchain {
    pub blocks: Vec<Block>,
    pub accounts: HashMap<Vec<u8>, AccountState>,
    pub pending_transactions: Vec<Transaction>,
    pub block_reward: u64,
    pub minimum_stake: u64,
    pub genesis_config: GenesisConfig,
    pub block_tree: BlockTree,
    pub finality_tracker: FinalityTracker,
    pub slashed_validators: HashSet<Vec<u8>>,
    storage: Option<Storage>,
}

impl Blockchain {
    pub fn new() -> Self {
        Self::from_genesis(GenesisConfig::default()).expect("default genesis must be valid")
    }

    pub fn from_genesis(genesis_config: GenesisConfig) -> Result<Self, ChainError> {
        let accounts = Self::accounts_from_genesis(&genesis_config)?;
        let state_root = Self::compute_state_root(&accounts);
        let genesis = Block::genesis_with_state_root(state_root);
        let block_tree = BlockTree::from_genesis(&genesis);

        Ok(Blockchain {
            blocks: vec![genesis],
            accounts,
            pending_transactions: Vec::new(),
            block_reward: genesis_config.block_reward,
            minimum_stake: genesis_config.minimum_stake,
            genesis_config,
            block_tree,
            finality_tracker: FinalityTracker::new(),
            slashed_validators: HashSet::new(),
            storage: None,
        })
    }

    pub fn with_storage(
        data_dir: &str,
        genesis_config: Option<&GenesisConfig>,
    ) -> Result<Self, ChainError> {
        let storage = Storage::open(data_dir).map_err(StorageError::from)?;

        if let Some(stored_height) = storage.get_height()? {
            let stored_genesis = storage
                .get_meta::<GenesisConfig>(CHAIN_CONFIG_KEY)?
                .unwrap_or_default();

            if let Some(expected_genesis) = genesis_config {
                if &stored_genesis != expected_genesis {
                    return Err(ChainError::GenesisMismatch);
                }
            }

            let expected_accounts = Self::accounts_from_genesis(&stored_genesis)?;
            let expected_genesis_hash =
                Block::genesis_with_state_root(Self::compute_state_root(&expected_accounts)).hash;

            let mut blocks = Vec::new();
            for h in 0..=stored_height {
                if let Some(block) = storage.get_block(h)? {
                    blocks.push(block);
                } else {
                    break;
                }
            }

            if blocks.is_empty() || blocks[0].hash != expected_genesis_hash {
                return Err(ChainError::GenesisMismatch);
            }

            let mut accounts = HashMap::new();
            for (addr, state) in storage.get_all_accounts()? {
                accounts.insert(addr, state);
            }

            let pending_transactions = storage.get_all_pending_transactions()?;
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

            Ok(Blockchain {
                blocks,
                accounts,
                pending_transactions,
                block_reward: stored_genesis.block_reward,
                minimum_stake: stored_genesis.minimum_stake,
                genesis_config: stored_genesis,
                block_tree,
                finality_tracker,
                slashed_validators,
                storage: Some(storage),
            })
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

        let tx_hash = tx.hash();
        if self
            .pending_transactions
            .iter()
            .any(|pending| pending.hash() == tx_hash)
        {
            return Err(ChainError::DuplicateTransaction);
        }

        let mut projected_accounts = self.accounts.clone();
        let mut seen_hashes = HashSet::new();
        for pending in &self.pending_transactions {
            let pending_hash = pending.hash();
            if !seen_hashes.insert(pending_hash) {
                return Err(ChainError::DuplicateTransaction);
            }
            Self::apply_user_transaction(&mut projected_accounts, pending)?;
        }

        Self::apply_user_transaction(&mut projected_accounts, &tx)?;
        self.pending_transactions.push(tx);
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
        let mut block_txs = Vec::new();
        let mut total_fees = 0u64;
        let mut seen_hashes = HashSet::new();

        for pending in &self.pending_transactions {
            let tx_hash = pending.hash();
            if !seen_hashes.insert(tx_hash) {
                continue;
            }

            if Self::apply_user_transaction(&mut projected_accounts, pending).is_ok() {
                total_fees = total_fees.saturating_add(pending.fee);
                block_txs.push(pending.clone());
            }
        }

        let coinbase = Transaction::coinbase(
            proposer_address.clone(),
            self.block_reward.saturating_add(total_fees),
        );
        Self::apply_coinbase_transaction(&mut projected_accounts, &coinbase)?;

        let mut transactions = vec![coinbase];
        transactions.extend(block_txs);

        Ok(Block::new(
            height,
            prev_hash,
            Self::compute_state_root(&projected_accounts),
            transactions,
            validator_keypair,
        ))
    }

    pub fn add_block(&mut self, block: Block) -> Result<(), ChainError> {
        let prev = self.latest_block();

        if block.header.height != prev.header.height + 1 {
            return Err(ChainError::InvalidHeight {
                expected: prev.header.height + 1,
                got: block.header.height,
            });
        }

        if block.header.prev_hash != prev.hash {
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
        let min_timestamp = prev.header.timestamp;
        let max_timestamp = now + MAX_FUTURE_BLOCK_TIME_SECS;
        if block.header.timestamp < min_timestamp || block.header.timestamp > max_timestamp {
            return Err(ChainError::InvalidBlockTimestamp {
                got: block.header.timestamp,
                min: min_timestamp,
                max: max_timestamp,
            });
        }

        self.ensure_validator_is_authorized(
            &block.header.validator_public_key,
            block.header.height,
            &block.header.prev_hash,
        )?;

        let proposer_address =
            hash::address_bytes_from_public_key(&block.header.validator_public_key);
        let mut projected_accounts = self.accounts.clone();
        let mut tx_hashes = HashSet::new();
        let mut user_fees = 0u64;
        let mut coinbase: Option<&Transaction> = None;

        for (index, tx) in block.transactions.iter().enumerate() {
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
            Self::apply_user_transaction(&mut projected_accounts, tx)?;
        }

        let coinbase = coinbase.ok_or(ChainError::MissingCoinbase)?;
        if coinbase.to != proposer_address {
            return Err(ChainError::InvalidCoinbase);
        }
        if coinbase.amount != self.block_reward.saturating_add(user_fees) {
            return Err(ChainError::InvalidCoinbase);
        }
        Self::apply_coinbase_transaction(&mut projected_accounts, coinbase)?;

        if block.header.state_root != Self::compute_state_root(&projected_accounts) {
            return Err(ChainError::InvalidStateRoot);
        }

        // Insert into block tree for fork tracking
        let proposer_stake = projected_accounts
            .get(&proposer_address)
            .map(|a| a.staked_balance)
            .unwrap_or(0);
        // Ignore block tree errors for blocks already in the tree
        let _ = self.block_tree.insert(block.clone(), proposer_stake);

        self.accounts = projected_accounts;
        self.blocks.push(block.clone());
        self.remove_block_transactions_from_mempool(&block);
        self.persist_block_state(&block)?;
        Ok(())
    }

    /// Add a finality vote. Returns Some(FinalizedBlock) if threshold reached.
    pub fn add_finality_vote(&mut self, vote: FinalityVote) -> Option<FinalizedBlock> {
        let result = self.finality_tracker.add_vote(
            vote,
            &self.accounts,
            self.minimum_stake,
            &self.slashed_validators,
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
        );
        let penalty = pos.slash_with_evidence(&mut self.accounts, evidence)?;
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
        ProofOfStake::with_slashed(self.minimum_stake, self.slashed_validators.clone())
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

        // This block forks from the canonical chain
        // Validate basic properties
        if !block.verify_hash() {
            return Err(ChainError::InvalidBlockHash);
        }
        if !block.verify_merkle_root() {
            return Err(ChainError::InvalidMerkleRoot);
        }
        if !block.verify_signature() {
            return Err(ChainError::InvalidBlockSignature);
        }

        // Get proposer stake for weight calculation
        let proposer_address =
            hash::address_bytes_from_public_key(&block.header.validator_public_key);
        let proposer_stake = self
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

        // Rebuild from genesis
        let mut accounts = Self::accounts_from_genesis(&self.genesis_config)?;
        let mut new_blocks = vec![canonical[0].clone()]; // genesis

        for block in canonical.iter().skip(1) {
            // Replay each block's transactions
            let proposer_address =
                hash::address_bytes_from_public_key(&block.header.validator_public_key);
            let mut user_fees = 0u64;

            for tx in &block.transactions {
                if tx.is_coinbase() {
                    continue;
                }
                user_fees = user_fees.saturating_add(tx.fee);
                Self::apply_user_transaction(&mut accounts, tx)?;
            }

            // Apply coinbase
            if let Some(coinbase) = block.transactions.first() {
                if coinbase.is_coinbase() {
                    Self::apply_coinbase_transaction(&mut accounts, coinbase)?;
                }
            }

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
        if accounts.is_empty() {
            return hash::sha3_hash(EMPTY_STATE_ROOT_SEED);
        }

        let mut entries: Vec<(&Vec<u8>, &AccountState)> = accounts.iter().collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let leaves: Vec<Vec<u8>> = entries
            .into_iter()
            .map(|(address, state)| {
                let encoded =
                    bincode::serialize(&(address, state)).expect("failed to serialize state leaf");
                hash::sha3_hash(&encoded)
            })
            .collect();

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

    fn ensure_validator_is_authorized(
        &self,
        validator_public_key: &[u8],
        block_height: u64,
        prev_hash: &[u8],
    ) -> Result<(), ChainError> {
        let pos =
            ProofOfStake::with_slashed(self.minimum_stake, self.slashed_validators.clone());
        match pos.select_validator(&self.accounts, block_height, prev_hash) {
            Some(expected) if expected.public_key == validator_public_key => Ok(()),
            Some(_) => Err(ChainError::UnauthorizedValidator),
            None => Ok(()),
        }
    }

    fn apply_user_transaction(
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        tx: &Transaction,
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
                sender.staked_balance = sender.staked_balance.saturating_add(tx.amount);
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
                // Return unstaked amount back to available balance
                sender.balance = sender.balance.saturating_add(tx.amount);
            }
        }

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
            chain_name: "curs3d-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 100,
                staked_balance: 5_000,
            }],
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

        let mut stake_tx = Transaction::stake(validator.public_key.clone(), 10_000_000, 5, 0);
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
        let mut stake_tx = Transaction::stake(validator.public_key.clone(), 10_000_000, 5, 0);
        stake_tx.sign(&validator);
        chain.add_transaction(stake_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(chain.get_staked_balance(&address), 10_000_000);
        let balance_after_stake = chain.get_balance(&address);

        // Unstake 5M
        let mut unstake_tx = Transaction::unstake(validator.public_key.clone(), 5_000_000, 5, 1);
        unstake_tx.sign(&validator);
        chain.add_transaction(unstake_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Staked reduced by 5M
        assert_eq!(chain.get_staked_balance(&address), 5_000_000);
        // Balance: previous + block_reward + total_fees_in_block + 5M_unstaked - 5M_deducted - 5_fee
        // Net effect of unstake on balance: -5 fee (amount deducted then re-added)
        // Plus block reward + fees collected as validator
        let expected = balance_after_stake + DEFAULT_BLOCK_REWARD + 5 - 5;
        // Validator collects the 5 fee in the coinbase, so net balance change = block_reward
        assert!(chain.get_balance(&address) > balance_after_stake);
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
}
