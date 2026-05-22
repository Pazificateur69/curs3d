pub mod backend;
pub use backend::{BlockBackend, InMemoryBlockBackend};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::consensus::{EpochSnapshot, EquivocationEvidence};
use crate::core::block::Block;
use crate::core::chain::{
    AccountState, DEFAULT_UNSTAKE_DELAY_BLOCKS, GenesisAllocation, GenesisConfig,
};
use crate::core::receipt::Receipt;
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::dilithium::Signature;
use crate::vm::state::ContractState;

const BLOCKS_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("blocks");
const STATE_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("accounts");
const META_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("meta");
const PENDING_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("pending");
const EVIDENCE_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("slashing_evidence");
const EPOCH_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("epochs");
const CONTRACT_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("contracts");
const RECEIPT_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("receipts");
const SNAPSHOT_MANIFEST_TABLE: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("snapshot_manifests");
const SNAPSHOT_CHUNK_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("snapshot_chunks");
pub const HEIGHT_KEY: &[u8] = b"chain_height";
pub const SCHEMA_VERSION_KEY: &[u8] = b"schema_version";
pub const CURRENT_SCHEMA_VERSION: u64 = 4;
pub const TOKEN_REGISTRY_KEY: &[u8] = b"token_registry";
pub const GOVERNANCE_STATE_KEY: &[u8] = b"governance_state";

// ─── State Sync Snapshot Types ──────────────────────────────────────

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SnapshotManifest {
    pub height: u64,
    pub epoch: u64,
    pub chain_id: String,
    pub genesis_hash: Vec<u8>,
    pub latest_hash: Vec<u8>,
    #[serde(default)]
    pub tip_height: u64,
    #[serde(default)]
    pub tip_hash: Vec<u8>,
    pub finalized_height: u64,
    pub finalized_hash: Vec<u8>,
    pub state_root: Vec<u8>,
    #[serde(default)]
    pub chunk_root: Vec<u8>,
    pub chunk_count: usize,
    pub chunk_hashes: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StateChunk {
    pub index: usize,
    pub data: Vec<u8>,
    pub hash: Vec<u8>,
    #[serde(default)]
    pub proof: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SnapshotState {
    pub blocks: Vec<Block>,
    pub accounts: Vec<(Vec<u8>, AccountState)>,
    pub contracts: Vec<(Vec<u8>, ContractState)>,
    pub receipts: Vec<(Vec<u8>, Receipt)>,
    pub pending_transactions: Vec<Transaction>,
    pub slashed_validators: Vec<Vec<u8>>,
    pub epoch_snapshots: Vec<(u64, EpochSnapshot)>,
    pub finalized_height: u64,
    pub finalized_hash: Vec<u8>,
}

#[derive(serde::Deserialize)]
struct LegacyGenesisConfigV1 {
    chain_name: String,
    block_reward: u64,
    minimum_stake: u64,
    allocations: Vec<GenesisAllocation>,
}

#[derive(serde::Deserialize)]
struct LegacyAccountStateV1 {
    balance: u64,
    nonce: u64,
    staked_balance: u64,
    public_key: Option<Vec<u8>>,
}

#[derive(serde::Deserialize)]
struct LegacyTransactionV1 {
    kind: TransactionKind,
    from: Vec<u8>,
    sender_public_key: Vec<u8>,
    to: Vec<u8>,
    amount: u64,
    fee: u64,
    nonce: u64,
    timestamp: i64,
    signature: Option<Signature>,
}

#[derive(serde::Deserialize)]
struct LegacyBlockV1 {
    header: crate::core::block::BlockHeader,
    transactions: Vec<LegacyTransactionV1>,
    hash: Vec<u8>,
    signature: Option<Signature>,
}

#[derive(serde::Deserialize)]
struct LegacyEquivocationEvidenceV1 {
    height: u64,
    validator_public_key: Vec<u8>,
    block_hash_a: Vec<u8>,
    signature_a: Signature,
    block_hash_b: Vec<u8>,
    signature_b: Signature,
}

impl From<LegacyGenesisConfigV1> for GenesisConfig {
    fn from(value: LegacyGenesisConfigV1) -> Self {
        GenesisConfig {
            chain_id: value.chain_name.clone(),
            chain_name: value.chain_name,
            block_reward: value.block_reward,
            minimum_stake: value.minimum_stake,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: crate::core::chain::DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: crate::core::chain::DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: value.allocations,
            ..Default::default()
        }
    }
}

impl From<LegacyAccountStateV1> for AccountState {
    fn from(value: LegacyAccountStateV1) -> Self {
        AccountState {
            balance: value.balance,
            nonce: value.nonce,
            staked_balance: value.staked_balance,
            pending_unstakes: Vec::new(),
            validator_active_from_height: 0,
            jailed_until_height: 0,
            public_key: value.public_key,
        }
    }
}

impl LegacyTransactionV1 {
    fn into_current(self, chain_id: &str) -> Transaction {
        Transaction {
            chain_id: chain_id.to_string(),
            kind: self.kind,
            from: self.from,
            sender_public_key: self.sender_public_key,
            to: self.to,
            amount: self.amount,
            fee: self.fee,
            max_fee_per_gas: self.fee,
            max_priority_fee_per_gas: self.fee,
            nonce: self.nonce,
            timestamp: self.timestamp,
            signature: self.signature,
            gas_limit: 0,
            data: Vec::new(),
            evm_raw_tx: Vec::new(),
        }
    }
}

impl LegacyBlockV1 {
    fn into_current(self, chain_id: &str) -> Block {
        Block {
            header: self.header,
            transactions: self
                .transactions
                .into_iter()
                .map(|tx| tx.into_current(chain_id))
                .collect(),
            hash: self.hash,
            signature: self.signature,
        }
    }
}

#[derive(Clone)]
pub struct Storage {
    db: Arc<Database>,
}

impl Storage {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        let db_path = Self::database_file(path.as_ref())?;
        let db = Database::create(&db_path).map_err(StorageError::redb)?;
        let storage = Storage { db: Arc::new(db) };
        storage.initialize_tables()?;
        Ok(storage)
    }

    fn database_file(path: &Path) -> Result<PathBuf, StorageError> {
        if path.extension().is_some_and(|ext| ext == "redb") {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| StorageError::Io(e.to_string()))?;
            }
            return Ok(path.to_path_buf());
        }
        std::fs::create_dir_all(path).map_err(|e| StorageError::Io(e.to_string()))?;
        Ok(path.join("curs3d.redb"))
    }

    fn initialize_tables(&self) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        {
            let _ = write.open_table(BLOCKS_TABLE).map_err(StorageError::redb)?;
            let _ = write.open_table(STATE_TABLE).map_err(StorageError::redb)?;
            let _ = write.open_table(META_TABLE).map_err(StorageError::redb)?;
            let _ = write
                .open_table(PENDING_TABLE)
                .map_err(StorageError::redb)?;
            let _ = write
                .open_table(EVIDENCE_TABLE)
                .map_err(StorageError::redb)?;
            let _ = write.open_table(EPOCH_TABLE).map_err(StorageError::redb)?;
            let _ = write
                .open_table(CONTRACT_TABLE)
                .map_err(StorageError::redb)?;
            let _ = write
                .open_table(RECEIPT_TABLE)
                .map_err(StorageError::redb)?;
            let _ = write
                .open_table(SNAPSHOT_MANIFEST_TABLE)
                .map_err(StorageError::redb)?;
            let _ = write
                .open_table(SNAPSHOT_CHUNK_TABLE)
                .map_err(StorageError::redb)?;
        }
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn put_block(&self, block: &Block) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let key = block.header.height.to_be_bytes();
        let value =
            bincode::serialize(block).map_err(|e| StorageError::Serialize(e.to_string()))?;
        {
            let mut blocks = write.open_table(BLOCKS_TABLE).map_err(StorageError::redb)?;
            blocks
                .insert(key.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
            let mut meta = write.open_table(META_TABLE).map_err(StorageError::redb)?;
            meta.insert(HEIGHT_KEY, key.as_slice())
                .map_err(StorageError::redb)?;
        }
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_block(&self, height: u64) -> Result<Option<Block>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(BLOCKS_TABLE).map_err(StorageError::redb)?;
        let key = height.to_be_bytes();
        match tree.get(key.as_slice()).map_err(StorageError::redb)? {
            Some(data) => {
                let block: Block = bincode::deserialize(data.value())
                    .map_err(|e| StorageError::Serialize(e.to_string()))?;
                Ok(Some(block))
            }
            None => Ok(None),
        }
    }

    pub fn get_block_compat(
        &self,
        height: u64,
        chain_id: &str,
    ) -> Result<Option<Block>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(BLOCKS_TABLE).map_err(StorageError::redb)?;
        let key = height.to_be_bytes();
        match tree.get(key.as_slice()).map_err(StorageError::redb)? {
            Some(data) => match bincode::deserialize::<Block>(data.value()) {
                Ok(block) => Ok(Some(block)),
                Err(_) => {
                    let block: LegacyBlockV1 = bincode::deserialize(data.value())
                        .map_err(|e| StorageError::Serialize(e.to_string()))?;
                    Ok(Some(block.into_current(chain_id)))
                }
            },
            None => Ok(None),
        }
    }

    /// Delete every block stored at height strictly below `retain_from`.
    /// Returns the count of blocks removed.
    ///
    /// SAFETY: caller MUST ensure all heights below `retain_from` are
    /// already finalised (and the in-memory chain reflects that), since
    /// pruned blocks can no longer be served to peers or replayed on
    /// boot. The intended caller passes
    /// `finalized_height.saturating_sub(retention_window)` so we never
    /// prune anything that could still be reorg-eligible.
    ///
    /// Runtime integration (calling this from inside the live chain
    /// loop after every finality advance) is deferred — the in-memory
    /// `Blockchain::blocks: Vec<Block>` is indexed by height and would
    /// need a base-offset refactor before holes are safe. This method
    /// ships now so the storage primitive is ready, callable from
    /// startup-time tooling and from unit tests, and so the live node
    /// can opt in once the chain-side refactor lands.
    pub fn prune_blocks_below(&self, retain_from: u64) -> Result<usize, StorageError> {
        if retain_from == 0 {
            return Ok(0);
        }
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let removed;
        {
            let mut blocks = write.open_table(BLOCKS_TABLE).map_err(StorageError::redb)?;
            // Collect keys to remove first so we don't iterate while
            // mutating. The BLOCKS_TABLE is keyed by u64 big-endian
            // height bytes, so a lexicographic range [0, retain_from)
            // is exactly the heights we want.
            let upper = retain_from.to_be_bytes();
            let keys_to_remove: Vec<[u8; 8]> = blocks
                .range::<&[u8]>(..upper.as_slice())
                .map_err(StorageError::redb)?
                .filter_map(|entry| entry.ok())
                .map(|(k, _)| {
                    let mut buf = [0u8; 8];
                    buf.copy_from_slice(k.value());
                    buf
                })
                .collect();
            removed = keys_to_remove.len();
            for key in keys_to_remove {
                blocks.remove(key.as_slice()).map_err(StorageError::redb)?;
            }
        }
        write.commit().map_err(StorageError::redb)?;
        Ok(removed)
    }

    pub fn get_height(&self) -> Result<Option<u64>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let meta = read.open_table(META_TABLE).map_err(StorageError::redb)?;
        match meta.get(HEIGHT_KEY).map_err(StorageError::redb)? {
            Some(data) => {
                let bytes: [u8; 8] = data
                    .value()
                    .try_into()
                    .map_err(|_| StorageError::Serialize("invalid height bytes".to_string()))?;
                Ok(Some(u64::from_be_bytes(bytes)))
            }
            None => Ok(None),
        }
    }

    pub fn put_account(&self, address: &[u8], state: &AccountState) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let value =
            bincode::serialize(state).map_err(|e| StorageError::Serialize(e.to_string()))?;
        {
            let mut tree = write.open_table(STATE_TABLE).map_err(StorageError::redb)?;
            tree.insert(address, value.as_slice())
                .map_err(StorageError::redb)?;
        }
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    pub fn replace_accounts(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
    ) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let mut tree = write.open_table(STATE_TABLE).map_err(StorageError::redb)?;
        tree.retain(|_, _| false).map_err(StorageError::redb)?;
        let mut entries: Vec<(&Vec<u8>, &AccountState)> = accounts.iter().collect();
        entries.sort_by_key(|(a, _)| *a);
        for (address, state) in entries {
            let value =
                bincode::serialize(state).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(address.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        drop(tree);
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_account(&self, address: &[u8]) -> Result<Option<AccountState>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(STATE_TABLE).map_err(StorageError::redb)?;
        match tree.get(address).map_err(StorageError::redb)? {
            Some(data) => {
                let state: AccountState = bincode::deserialize(data.value())
                    .map_err(|e| StorageError::Serialize(e.to_string()))?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub fn get_account_compat(&self, address: &[u8]) -> Result<Option<AccountState>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(STATE_TABLE).map_err(StorageError::redb)?;
        match tree.get(address).map_err(StorageError::redb)? {
            Some(data) => match bincode::deserialize::<AccountState>(data.value()) {
                Ok(state) => Ok(Some(state)),
                Err(_) => {
                    let state: LegacyAccountStateV1 = bincode::deserialize(data.value())
                        .map_err(|e| StorageError::Serialize(e.to_string()))?;
                    Ok(Some(state.into()))
                }
            },
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub fn get_all_accounts(&self) -> Result<Vec<(Vec<u8>, AccountState)>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(STATE_TABLE).map_err(StorageError::redb)?;
        let mut accounts = Vec::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (key, value) = entry.map_err(StorageError::redb)?;
            let state: AccountState = bincode::deserialize(value.value())
                .map_err(|e| StorageError::Serialize(e.to_string()))?;
            accounts.push((key.value().to_vec(), state));
        }
        Ok(accounts)
    }

    pub fn get_all_accounts_compat(&self) -> Result<Vec<(Vec<u8>, AccountState)>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(STATE_TABLE).map_err(StorageError::redb)?;
        let mut accounts = Vec::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (key, value) = entry.map_err(StorageError::redb)?;
            let state = match bincode::deserialize::<AccountState>(value.value()) {
                Ok(state) => state,
                Err(_) => {
                    let legacy: LegacyAccountStateV1 = bincode::deserialize(value.value())
                        .map_err(|e| StorageError::Serialize(e.to_string()))?;
                    legacy.into()
                }
            };
            accounts.push((key.value().to_vec(), state));
        }
        Ok(accounts)
    }

    pub fn replace_pending_transactions(&self, txs: &[Transaction]) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let mut tree = write
            .open_table(PENDING_TABLE)
            .map_err(StorageError::redb)?;
        tree.retain(|_, _| false).map_err(StorageError::redb)?;
        for tx in txs {
            let key = tx.hash();
            let value =
                bincode::serialize(tx).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(key.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        drop(tree);
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_all_pending_transactions(&self) -> Result<Vec<Transaction>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(PENDING_TABLE).map_err(StorageError::redb)?;
        let mut txs = Vec::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (_key, value) = entry.map_err(StorageError::redb)?;
            let tx: Transaction = bincode::deserialize(value.value())
                .map_err(|e| StorageError::Serialize(e.to_string()))?;
            txs.push(tx);
        }
        txs.sort_by_key(|tx| (tx.timestamp, tx.nonce));
        Ok(txs)
    }

    pub fn replace_blocks(&self, blocks: &[Block]) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let mut tree = write.open_table(BLOCKS_TABLE).map_err(StorageError::redb)?;
        tree.retain(|_, _| false).map_err(StorageError::redb)?;
        for block in blocks {
            let key = block.header.height.to_be_bytes();
            let value =
                bincode::serialize(block).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(key.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        drop(tree);
        let mut meta = write.open_table(META_TABLE).map_err(StorageError::redb)?;
        if let Some(last) = blocks.last() {
            let key = last.header.height.to_be_bytes();
            meta.insert(HEIGHT_KEY, key.as_slice())
                .map_err(StorageError::redb)?;
        } else {
            meta.remove(HEIGHT_KEY).map_err(StorageError::redb)?;
        }
        drop(meta);
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    pub fn replace_contracts(
        &self,
        contracts: &HashMap<Vec<u8>, ContractState>,
    ) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let mut tree = write
            .open_table(CONTRACT_TABLE)
            .map_err(StorageError::redb)?;
        tree.retain(|_, _| false).map_err(StorageError::redb)?;
        let mut entries: Vec<(&Vec<u8>, &ContractState)> = contracts.iter().collect();
        entries.sort_by_key(|(a, _)| *a);
        for (address, contract) in entries {
            let value =
                bincode::serialize(contract).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(address.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        drop(tree);
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_all_contracts(&self) -> Result<Vec<(Vec<u8>, ContractState)>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read
            .open_table(CONTRACT_TABLE)
            .map_err(StorageError::redb)?;
        let mut contracts = Vec::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (key, value) = entry.map_err(StorageError::redb)?;
            let contract: ContractState = bincode::deserialize(value.value())
                .map_err(|e| StorageError::Serialize(e.to_string()))?;
            contracts.push((key.value().to_vec(), contract));
        }
        Ok(contracts)
    }

    pub fn replace_receipts(
        &self,
        receipts: &HashMap<Vec<u8>, Receipt>,
    ) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let mut tree = write
            .open_table(RECEIPT_TABLE)
            .map_err(StorageError::redb)?;
        tree.retain(|_, _| false).map_err(StorageError::redb)?;
        let mut entries: Vec<(&Vec<u8>, &Receipt)> = receipts.iter().collect();
        entries.sort_by_key(|(a, _)| *a);
        for (tx_hash, receipt) in entries {
            let value =
                bincode::serialize(receipt).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(tx_hash.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        drop(tree);
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_all_receipts(&self) -> Result<Vec<(Vec<u8>, Receipt)>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(RECEIPT_TABLE).map_err(StorageError::redb)?;
        let mut receipts = Vec::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (key, value) = entry.map_err(StorageError::redb)?;
            let receipt: Receipt = bincode::deserialize(value.value())
                .map_err(|e| StorageError::Serialize(e.to_string()))?;
            receipts.push((key.value().to_vec(), receipt));
        }
        Ok(receipts)
    }

    pub fn get_all_pending_transactions_compat(
        &self,
        chain_id: &str,
    ) -> Result<Vec<Transaction>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(PENDING_TABLE).map_err(StorageError::redb)?;
        let mut txs = Vec::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (_key, value) = entry.map_err(StorageError::redb)?;
            let tx = match bincode::deserialize::<Transaction>(value.value()) {
                Ok(tx) => tx,
                Err(_) => {
                    let legacy: LegacyTransactionV1 = bincode::deserialize(value.value())
                        .map_err(|e| StorageError::Serialize(e.to_string()))?;
                    legacy.into_current(chain_id)
                }
            };
            txs.push(tx);
        }
        txs.sort_by_key(|tx| (tx.timestamp, tx.nonce));
        Ok(txs)
    }

    pub fn put_evidence(&self, evidence: &EquivocationEvidence) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let key = evidence.key();
        let value =
            bincode::serialize(evidence).map_err(|e| StorageError::Serialize(e.to_string()))?;
        {
            let mut tree = write
                .open_table(EVIDENCE_TABLE)
                .map_err(StorageError::redb)?;
            tree.insert(key.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_all_evidence(&self) -> Result<Vec<EquivocationEvidence>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read
            .open_table(EVIDENCE_TABLE)
            .map_err(StorageError::redb)?;
        let mut evidence_list = Vec::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (_key, value) = entry.map_err(StorageError::redb)?;
            let evidence: EquivocationEvidence = bincode::deserialize(value.value())
                .map_err(|e| StorageError::Serialize(e.to_string()))?;
            evidence_list.push(evidence);
        }
        Ok(evidence_list)
    }

    /// Get set of slashed validator addresses from stored evidence
    pub fn get_slashed_addresses(
        &self,
    ) -> Result<std::collections::HashSet<Vec<u8>>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read
            .open_table(EVIDENCE_TABLE)
            .map_err(StorageError::redb)?;
        let mut addresses = std::collections::HashSet::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (_key, value) = entry.map_err(StorageError::redb)?;
            if let Ok(evidence) = bincode::deserialize::<EquivocationEvidence>(value.value()) {
                addresses.insert(crate::crypto::hash::address_bytes_from_public_key(
                    &evidence.validator_public_key,
                ));
                continue;
            }

            let legacy: LegacyEquivocationEvidenceV1 = bincode::deserialize(value.value())
                .map_err(|e| StorageError::Serialize(e.to_string()))?;
            let _ = (
                legacy.height,
                legacy.block_hash_a,
                legacy.signature_a,
                legacy.block_hash_b,
                legacy.signature_b,
            );
            addresses.insert(crate::crypto::hash::address_bytes_from_public_key(
                &legacy.validator_public_key,
            ));
        }
        Ok(addresses)
    }

    pub fn get_schema_version(&self) -> Result<Option<u64>, StorageError> {
        self.get_meta(SCHEMA_VERSION_KEY)
    }

    pub fn get_genesis_config_compat(
        &self,
        key: &[u8],
    ) -> Result<Option<GenesisConfig>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let meta = read.open_table(META_TABLE).map_err(StorageError::redb)?;
        match meta.get(key).map_err(StorageError::redb)? {
            Some(data) => match bincode::deserialize::<GenesisConfig>(data.value()) {
                Ok(value) => Ok(Some(value)),
                Err(_) => {
                    let legacy: LegacyGenesisConfigV1 = bincode::deserialize(data.value())
                        .map_err(|e| StorageError::Serialize(e.to_string()))?;
                    Ok(Some(legacy.into()))
                }
            },
            None => Ok(None),
        }
    }

    pub fn put_meta<T: Serialize>(&self, key: &[u8], value: &T) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let data = bincode::serialize(value).map_err(|e| StorageError::Serialize(e.to_string()))?;
        {
            let mut meta = write.open_table(META_TABLE).map_err(StorageError::redb)?;
            meta.insert(key, data.as_slice())
                .map_err(StorageError::redb)?;
        }
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    pub fn get_meta<T: DeserializeOwned>(&self, key: &[u8]) -> Result<Option<T>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let meta = read.open_table(META_TABLE).map_err(StorageError::redb)?;
        match meta.get(key).map_err(StorageError::redb)? {
            Some(data) => {
                let value: T = bincode::deserialize(data.value())
                    .map_err(|e| StorageError::Serialize(e.to_string()))?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    // ─── Epoch Snapshot Persistence ─────────────────────────────────

    #[allow(dead_code)]
    pub fn put_epoch_snapshot(
        &self,
        epoch: u64,
        snapshot: &EpochSnapshot,
    ) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let key = epoch.to_be_bytes();
        let value =
            bincode::serialize(snapshot).map_err(|e| StorageError::Serialize(e.to_string()))?;
        {
            let mut tree = write.open_table(EPOCH_TABLE).map_err(StorageError::redb)?;
            tree.insert(key.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    pub fn replace_epoch_snapshots(
        &self,
        snapshots: &HashMap<u64, EpochSnapshot>,
    ) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let mut tree = write.open_table(EPOCH_TABLE).map_err(StorageError::redb)?;
        tree.retain(|_, _| false).map_err(StorageError::redb)?;
        let mut entries: Vec<(&u64, &EpochSnapshot)> = snapshots.iter().collect();
        entries.sort_by_key(|(epoch, _)| **epoch);
        for (epoch, snapshot) in entries {
            let key = epoch.to_be_bytes();
            let value =
                bincode::serialize(snapshot).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(key.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        drop(tree);
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_epoch_snapshot(&self, epoch: u64) -> Result<Option<EpochSnapshot>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(EPOCH_TABLE).map_err(StorageError::redb)?;
        let key = epoch.to_be_bytes();
        match tree.get(key.as_slice()).map_err(StorageError::redb)? {
            Some(data) => {
                let snapshot: EpochSnapshot = bincode::deserialize(data.value())
                    .map_err(|e| StorageError::Serialize(e.to_string()))?;
                Ok(Some(snapshot))
            }
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub fn get_all_epoch_snapshots(&self) -> Result<Vec<(u64, EpochSnapshot)>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read.open_table(EPOCH_TABLE).map_err(StorageError::redb)?;
        let mut snapshots = Vec::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (key, value) = entry.map_err(StorageError::redb)?;
            let epoch_bytes: [u8; 8] = key
                .value()
                .try_into()
                .map_err(|_| StorageError::Serialize("invalid epoch key".to_string()))?;
            let epoch = u64::from_be_bytes(epoch_bytes);
            let snapshot: EpochSnapshot = bincode::deserialize(value.value())
                .map_err(|e| StorageError::Serialize(e.to_string()))?;
            snapshots.push((epoch, snapshot));
        }
        Ok(snapshots)
    }

    // ─── State Sync Snapshot Persistence ────────────────────────────

    pub fn put_snapshot_manifest(
        &self,
        height: u64,
        manifest: &SnapshotManifest,
    ) -> Result<(), StorageError> {
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let key = height.to_be_bytes();
        let value =
            bincode::serialize(manifest).map_err(|e| StorageError::Serialize(e.to_string()))?;
        {
            let mut tree = write
                .open_table(SNAPSHOT_MANIFEST_TABLE)
                .map_err(StorageError::redb)?;
            tree.insert(key.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_latest_snapshot_manifest(&self) -> Result<Option<SnapshotManifest>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read
            .open_table(SNAPSHOT_MANIFEST_TABLE)
            .map_err(StorageError::redb)?;
        match tree.last().map_err(StorageError::redb)? {
            Some((_key, value)) => {
                let manifest: SnapshotManifest = bincode::deserialize(value.value())
                    .map_err(|e| StorageError::Serialize(e.to_string()))?;
                Ok(Some(manifest))
            }
            None => Ok(None),
        }
    }

    pub fn put_snapshot_chunk(
        &self,
        height: u64,
        chunk: &StateChunk,
        expected_chunk_count: Option<usize>,
    ) -> Result<(), StorageError> {
        // Validate chunk index against manifest to prevent storage bloat attacks
        if let Some(count) = expected_chunk_count
            && chunk.index >= count
        {
            return Err(StorageError::Serialize(format!(
                "chunk index {} exceeds expected count {}",
                chunk.index, count
            )));
        }
        let write = self.db.begin_write().map_err(StorageError::redb)?;
        let mut key = height.to_be_bytes().to_vec();
        key.extend_from_slice(&(chunk.index as u64).to_be_bytes());
        let value =
            bincode::serialize(chunk).map_err(|e| StorageError::Serialize(e.to_string()))?;
        {
            let mut tree = write
                .open_table(SNAPSHOT_CHUNK_TABLE)
                .map_err(StorageError::redb)?;
            tree.insert(key.as_slice(), value.as_slice())
                .map_err(StorageError::redb)?;
        }
        write.commit().map_err(StorageError::redb)?;
        Ok(())
    }

    pub fn get_snapshot_chunks(&self, height: u64) -> Result<Vec<StateChunk>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read
            .open_table(SNAPSHOT_CHUNK_TABLE)
            .map_err(StorageError::redb)?;
        let prefix = height.to_be_bytes();
        let mut chunks = Vec::new();
        for entry in tree.iter().map_err(StorageError::redb)? {
            let (key, value) = entry.map_err(StorageError::redb)?;
            if !key.value().starts_with(&prefix) {
                continue;
            }
            let chunk: StateChunk = bincode::deserialize(value.value())
                .map_err(|e| StorageError::Serialize(e.to_string()))?;
            chunks.push(chunk);
        }
        chunks.sort_by_key(|chunk| chunk.index);
        Ok(chunks)
    }

    #[allow(dead_code)]
    pub fn get_snapshot_chunk(
        &self,
        height: u64,
        index: usize,
    ) -> Result<Option<StateChunk>, StorageError> {
        let read = self.db.begin_read().map_err(StorageError::redb)?;
        let tree = read
            .open_table(SNAPSHOT_CHUNK_TABLE)
            .map_err(StorageError::redb)?;
        let mut key = height.to_be_bytes().to_vec();
        key.extend_from_slice(&(index as u64).to_be_bytes());
        match tree.get(key.as_slice()).map_err(StorageError::redb)? {
            Some(data) => {
                let chunk: StateChunk = bincode::deserialize(data.value())
                    .map_err(|e| StorageError::Serialize(e.to_string()))?;
                Ok(Some(chunk))
            }
            None => Ok(None),
        }
    }

    pub fn flush(&self) -> Result<(), StorageError> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("redb error: {0}")]
    Redb(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("serialization error: {0}")]
    Serialize(String),
}

impl StorageError {
    fn redb(error: impl std::fmt::Display) -> Self {
        StorageError::Redb(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::block::Block;
    use crate::core::chain::GenesisConfig;

    #[test]
    fn test_store_and_load_block() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();

        let genesis = Block::genesis();
        storage.put_block(&genesis).unwrap();

        let loaded = storage.get_block(0).unwrap().unwrap();
        assert_eq!(loaded.header.height, 0);
        assert_eq!(loaded.hash, genesis.hash);
    }

    /// Synthesize a Block at a given height for storage-only tests.
    /// The block is NOT validated by the chain (the header is mutated
    /// post-construction, so the hash is stale) — these tests exercise
    /// the storage byte-shuffling layer, not consensus validation.
    fn synthetic_block_at_height(height: u64) -> Block {
        let mut b = Block::genesis();
        b.header.height = height;
        b
    }

    #[test]
    fn prune_blocks_below_removes_only_lower_heights() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();
        for height in 0..20u64 {
            storage
                .put_block(&synthetic_block_at_height(height))
                .unwrap();
        }

        let removed = storage.prune_blocks_below(10).unwrap();
        assert_eq!(removed, 10, "should prune heights 0..10 (10 entries)");

        // Heights 0..10 gone.
        for h in 0..10u64 {
            assert!(
                storage.get_block(h).unwrap().is_none(),
                "block at height {} should be pruned",
                h
            );
        }
        // Heights 10..20 intact.
        for h in 10..20u64 {
            assert!(
                storage.get_block(h).unwrap().is_some(),
                "block at height {} must NOT be pruned (retain_from=10)",
                h
            );
        }
    }

    #[test]
    fn prune_blocks_below_zero_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();
        for height in 0..5u64 {
            storage
                .put_block(&synthetic_block_at_height(height))
                .unwrap();
        }
        let removed = storage.prune_blocks_below(0).unwrap();
        assert_eq!(removed, 0);
        for h in 0..5u64 {
            assert!(storage.get_block(h).unwrap().is_some());
        }
    }

    #[test]
    fn prune_blocks_below_handles_empty_table() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();
        let removed = storage.prune_blocks_below(100).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn prune_blocks_below_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();
        for height in 0..10u64 {
            storage
                .put_block(&synthetic_block_at_height(height))
                .unwrap();
        }
        assert_eq!(storage.prune_blocks_below(5).unwrap(), 5);
        // Second call removes nothing — the heights are already gone.
        assert_eq!(storage.prune_blocks_below(5).unwrap(), 0);
    }

    #[test]
    fn test_store_and_load_account() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();

        let addr = vec![1; 32];
        let state = AccountState {
            balance: 5000,
            nonce: 3,
            staked_balance: 0,
            pending_unstakes: Vec::new(),
            validator_active_from_height: 0,
            jailed_until_height: 0,
            public_key: None,
        };
        storage.put_account(&addr, &state).unwrap();

        let loaded = storage.get_account(&addr).unwrap().unwrap();
        assert_eq!(loaded.balance, 5000);
        assert_eq!(loaded.nonce, 3);
    }

    #[test]
    fn test_height_tracking() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();

        assert!(storage.get_height().unwrap().is_none());

        let genesis = Block::genesis();
        storage.put_block(&genesis).unwrap();
        assert_eq!(storage.get_height().unwrap(), Some(0));
    }

    #[test]
    fn test_pending_transactions_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();

        let tx = Transaction::coinbase(
            "curs3d-devnet",
            vec![1; crate::crypto::hash::ADDRESS_LEN],
            50,
        );
        storage
            .replace_pending_transactions(std::slice::from_ref(&tx))
            .unwrap();

        let pending = storage.get_all_pending_transactions().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash(), tx.hash());
    }

    #[test]
    fn test_meta_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();

        let genesis = GenesisConfig::default();
        storage.put_meta(b"genesis", &genesis).unwrap();
        let loaded = storage.get_meta::<GenesisConfig>(b"genesis").unwrap();
        assert_eq!(loaded, Some(genesis));
    }

    #[test]
    fn test_epoch_snapshot_roundtrip() {
        use crate::consensus::{EpochSnapshot, Validator};

        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();

        let snapshot = EpochSnapshot {
            epoch: 5,
            start_height: 160,
            validators: vec![Validator {
                address: vec![1; 20],
                public_key: vec![2; 32],
                stake: 10_000,
            }],
            total_stake: 10_000,
        };

        storage.put_epoch_snapshot(5, &snapshot).unwrap();
        let loaded = storage.get_epoch_snapshot(5).unwrap().unwrap();
        assert_eq!(loaded.epoch, 5);
        assert_eq!(loaded.start_height, 160);
        assert_eq!(loaded.validators.len(), 1);
        assert_eq!(loaded.total_stake, 10_000);

        // Non-existent epoch
        assert!(storage.get_epoch_snapshot(99).unwrap().is_none());

        // get_all_epoch_snapshots
        let all = storage.get_all_epoch_snapshots().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, 5);
    }

    #[test]
    fn test_snapshot_manifest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("test_db")).unwrap();

        let manifest = SnapshotManifest {
            height: 100,
            epoch: 3,
            chain_id: "curs3d-test".to_string(),
            genesis_hash: vec![0x11; 32],
            latest_hash: vec![0x22; 32],
            tip_height: 123,
            tip_hash: vec![0x44; 32],
            finalized_height: 96,
            finalized_hash: vec![0x33; 32],
            state_root: vec![0xAA; 32],
            chunk_root: vec![0xDD; 32],
            chunk_count: 2,
            chunk_hashes: vec![vec![0xBB; 32], vec![0xCC; 32]],
        };

        storage.put_snapshot_manifest(100, &manifest).unwrap();
        let loaded = storage.get_latest_snapshot_manifest().unwrap().unwrap();
        assert_eq!(loaded.height, 100);
        assert_eq!(loaded.tip_height, 123);
        assert_eq!(loaded.chunk_root, vec![0xDD; 32]);
        assert_eq!(loaded.chunk_count, 2);

        let chunk = StateChunk {
            index: 0,
            data: vec![1, 2, 3, 4],
            hash: vec![0xBB; 32],
            proof: vec![vec![0xCC; 32]],
        };
        storage.put_snapshot_chunk(100, &chunk, None).unwrap();
        let loaded_chunk = storage.get_snapshot_chunk(100, 0).unwrap().unwrap();
        assert_eq!(loaded_chunk.index, 0);
        assert_eq!(loaded_chunk.data, vec![1, 2, 3, 4]);
        assert_eq!(loaded_chunk.proof.len(), 1);
    }

    // ─── Property tests (task #33) — redb roundtrip + prune ─────────

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]

        /// put_block then get_block round-trip preserves byte-identity
        /// for any height. Catches any encoding drift in redb writer/reader.
        #[test]
        fn prop_block_roundtrip(height in 0u64..1024) {
            let dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(dir.path().join("rt_db")).unwrap();
            let mut b = Block::genesis();
            b.header.height = height;
            b.header.gas_used = height; // any value, just to vary the bytes
            storage.put_block(&b).unwrap();
            let read = storage.get_block(height).unwrap().expect("present");
            prop_assert_eq!(read.header.height, height);
            prop_assert_eq!(read.header.gas_used, height);
        }

        /// After prune_blocks_below(K) for any K, every height < K reports
        /// None and every height >= K (that was originally stored) reports Some.
        /// Prune semantics must be a clean threshold cut — no leaks, no
        /// over-aggressive removal.
        #[test]
        fn prop_prune_threshold_clean(
            count in 5u8..30,
            keep_from in 1u64..15,
        ) {
            let dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(dir.path().join("prune_db")).unwrap();
            for h in 0..count as u64 {
                storage.put_block(&synthetic_block_at_height(h)).unwrap();
            }
            let keep = keep_from.min(count as u64);
            storage.prune_blocks_below(keep).unwrap();
            for h in 0..keep {
                prop_assert!(storage.get_block(h).unwrap().is_none(),
                    "pruned height {h} should be None");
            }
            for h in keep..count as u64 {
                prop_assert!(storage.get_block(h).unwrap().is_some(),
                    "non-pruned height {h} should be Some");
            }
        }

        /// put_block overwrites: writing twice at the same height with
        /// different content keeps the latest write.
        #[test]
        fn prop_put_block_overwrites(height in 0u64..256) {
            let dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(dir.path().join("overwrite_db")).unwrap();
            let mut b1 = synthetic_block_at_height(height);
            b1.header.gas_used = 1;
            let mut b2 = synthetic_block_at_height(height);
            b2.header.gas_used = 2;
            storage.put_block(&b1).unwrap();
            storage.put_block(&b2).unwrap();
            let read = storage.get_block(height).unwrap().expect("present");
            prop_assert_eq!(read.header.gas_used, 2);
        }
    }
}
