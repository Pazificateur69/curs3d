use serde::{Serialize, de::DeserializeOwned};
use sled::Db;
use std::collections::HashMap;
use std::path::Path;

use crate::consensus::{EpochSnapshot, EquivocationEvidence};
use crate::core::block::Block;
use crate::core::chain::{
    AccountState, DEFAULT_UNSTAKE_DELAY_BLOCKS, GenesisAllocation, GenesisConfig,
};
use crate::core::receipt::Receipt;
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::dilithium::Signature;
use crate::vm::state::ContractState;

const BLOCKS_TREE: &str = "blocks";
const STATE_TREE: &str = "accounts";
const META_TREE: &str = "meta";
const PENDING_TREE: &str = "pending";
const EVIDENCE_TREE: &str = "slashing_evidence";
const EPOCH_TREE: &str = "epochs";
const CONTRACT_TREE: &str = "contracts";
const RECEIPT_TREE: &str = "receipts";
const SNAPSHOT_MANIFEST_TREE: &str = "snapshot_manifests";
const SNAPSHOT_CHUNK_TREE: &str = "snapshot_chunks";
const HEIGHT_KEY: &[u8] = b"chain_height";
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

pub struct Storage {
    db: Db,
}

impl Storage {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, sled::Error> {
        let db = sled::open(path)?;
        Ok(Storage { db })
    }

    #[allow(dead_code)]
    pub fn put_block(&self, block: &Block) -> Result<(), StorageError> {
        let tree = self.db.open_tree(BLOCKS_TREE)?;
        let key = block.header.height.to_be_bytes();
        let value =
            bincode::serialize(block).map_err(|e| StorageError::Serialize(e.to_string()))?;
        tree.insert(key, value)?;

        let meta = self.db.open_tree(META_TREE)?;
        meta.insert(HEIGHT_KEY, &block.header.height.to_be_bytes())?;

        self.db.flush()?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_block(&self, height: u64) -> Result<Option<Block>, StorageError> {
        let tree = self.db.open_tree(BLOCKS_TREE)?;
        let key = height.to_be_bytes();
        match tree.get(key)? {
            Some(data) => {
                let block: Block = bincode::deserialize(&data)
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
        let tree = self.db.open_tree(BLOCKS_TREE)?;
        let key = height.to_be_bytes();
        match tree.get(key)? {
            Some(data) => match bincode::deserialize::<Block>(&data) {
                Ok(block) => Ok(Some(block)),
                Err(_) => {
                    let block: LegacyBlockV1 = bincode::deserialize(&data)
                        .map_err(|e| StorageError::Serialize(e.to_string()))?;
                    Ok(Some(block.into_current(chain_id)))
                }
            },
            None => Ok(None),
        }
    }

    pub fn get_height(&self) -> Result<Option<u64>, StorageError> {
        let meta = self.db.open_tree(META_TREE)?;
        match meta.get(HEIGHT_KEY)? {
            Some(data) => {
                let bytes: [u8; 8] = data
                    .as_ref()
                    .try_into()
                    .map_err(|_| StorageError::Serialize("invalid height bytes".to_string()))?;
                Ok(Some(u64::from_be_bytes(bytes)))
            }
            None => Ok(None),
        }
    }

    pub fn put_account(&self, address: &[u8], state: &AccountState) -> Result<(), StorageError> {
        let tree = self.db.open_tree(STATE_TREE)?;
        let value =
            bincode::serialize(state).map_err(|e| StorageError::Serialize(e.to_string()))?;
        tree.insert(address, value)?;
        Ok(())
    }

    pub fn replace_accounts(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
    ) -> Result<(), StorageError> {
        let tree = self.db.open_tree(STATE_TREE)?;
        tree.clear()?;
        let mut entries: Vec<(&Vec<u8>, &AccountState)> = accounts.iter().collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (address, state) in entries {
            let value =
                bincode::serialize(state).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(address, value)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_account(&self, address: &[u8]) -> Result<Option<AccountState>, StorageError> {
        let tree = self.db.open_tree(STATE_TREE)?;
        match tree.get(address)? {
            Some(data) => {
                let state: AccountState = bincode::deserialize(&data)
                    .map_err(|e| StorageError::Serialize(e.to_string()))?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub fn get_account_compat(&self, address: &[u8]) -> Result<Option<AccountState>, StorageError> {
        let tree = self.db.open_tree(STATE_TREE)?;
        match tree.get(address)? {
            Some(data) => match bincode::deserialize::<AccountState>(&data) {
                Ok(state) => Ok(Some(state)),
                Err(_) => {
                    let state: LegacyAccountStateV1 = bincode::deserialize(&data)
                        .map_err(|e| StorageError::Serialize(e.to_string()))?;
                    Ok(Some(state.into()))
                }
            },
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub fn get_all_accounts(&self) -> Result<Vec<(Vec<u8>, AccountState)>, StorageError> {
        let tree = self.db.open_tree(STATE_TREE)?;
        let mut accounts = Vec::new();
        for entry in tree.iter() {
            let (key, value) = entry?;
            let state: AccountState =
                bincode::deserialize(&value).map_err(|e| StorageError::Serialize(e.to_string()))?;
            accounts.push((key.to_vec(), state));
        }
        Ok(accounts)
    }

    pub fn get_all_accounts_compat(&self) -> Result<Vec<(Vec<u8>, AccountState)>, StorageError> {
        let tree = self.db.open_tree(STATE_TREE)?;
        let mut accounts = Vec::new();
        for entry in tree.iter() {
            let (key, value) = entry?;
            let state = match bincode::deserialize::<AccountState>(&value) {
                Ok(state) => state,
                Err(_) => {
                    let legacy: LegacyAccountStateV1 = bincode::deserialize(&value)
                        .map_err(|e| StorageError::Serialize(e.to_string()))?;
                    legacy.into()
                }
            };
            accounts.push((key.to_vec(), state));
        }
        Ok(accounts)
    }

    pub fn replace_pending_transactions(&self, txs: &[Transaction]) -> Result<(), StorageError> {
        let tree = self.db.open_tree(PENDING_TREE)?;
        tree.clear()?;
        for tx in txs {
            let key = tx.hash();
            let value =
                bincode::serialize(tx).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(key, value)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_all_pending_transactions(&self) -> Result<Vec<Transaction>, StorageError> {
        let tree = self.db.open_tree(PENDING_TREE)?;
        let mut txs = Vec::new();
        for entry in tree.iter() {
            let (_key, value) = entry?;
            let tx: Transaction =
                bincode::deserialize(&value).map_err(|e| StorageError::Serialize(e.to_string()))?;
            txs.push(tx);
        }
        txs.sort_by_key(|tx| (tx.timestamp, tx.nonce));
        Ok(txs)
    }

    pub fn replace_blocks(&self, blocks: &[Block]) -> Result<(), StorageError> {
        let tree = self.db.open_tree(BLOCKS_TREE)?;
        tree.clear()?;
        let meta = self.db.open_tree(META_TREE)?;
        for block in blocks {
            let key = block.header.height.to_be_bytes();
            let value =
                bincode::serialize(block).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(key, value)?;
        }
        if let Some(last) = blocks.last() {
            meta.insert(HEIGHT_KEY, &last.header.height.to_be_bytes())?;
        } else {
            meta.remove(HEIGHT_KEY)?;
        }
        Ok(())
    }

    pub fn replace_contracts(
        &self,
        contracts: &HashMap<Vec<u8>, ContractState>,
    ) -> Result<(), StorageError> {
        let tree = self.db.open_tree(CONTRACT_TREE)?;
        tree.clear()?;
        let mut entries: Vec<(&Vec<u8>, &ContractState)> = contracts.iter().collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (address, contract) in entries {
            let value =
                bincode::serialize(contract).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(address, value)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_all_contracts(&self) -> Result<Vec<(Vec<u8>, ContractState)>, StorageError> {
        let tree = self.db.open_tree(CONTRACT_TREE)?;
        let mut contracts = Vec::new();
        for entry in tree.iter() {
            let (key, value) = entry?;
            let contract: ContractState =
                bincode::deserialize(&value).map_err(|e| StorageError::Serialize(e.to_string()))?;
            contracts.push((key.to_vec(), contract));
        }
        Ok(contracts)
    }

    pub fn replace_receipts(
        &self,
        receipts: &HashMap<Vec<u8>, Receipt>,
    ) -> Result<(), StorageError> {
        let tree = self.db.open_tree(RECEIPT_TREE)?;
        tree.clear()?;
        let mut entries: Vec<(&Vec<u8>, &Receipt)> = receipts.iter().collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));
        for (tx_hash, receipt) in entries {
            let value =
                bincode::serialize(receipt).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(tx_hash, value)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_all_receipts(&self) -> Result<Vec<(Vec<u8>, Receipt)>, StorageError> {
        let tree = self.db.open_tree(RECEIPT_TREE)?;
        let mut receipts = Vec::new();
        for entry in tree.iter() {
            let (key, value) = entry?;
            let receipt: Receipt =
                bincode::deserialize(&value).map_err(|e| StorageError::Serialize(e.to_string()))?;
            receipts.push((key.to_vec(), receipt));
        }
        Ok(receipts)
    }

    pub fn get_all_pending_transactions_compat(
        &self,
        chain_id: &str,
    ) -> Result<Vec<Transaction>, StorageError> {
        let tree = self.db.open_tree(PENDING_TREE)?;
        let mut txs = Vec::new();
        for entry in tree.iter() {
            let (_key, value) = entry?;
            let tx = match bincode::deserialize::<Transaction>(&value) {
                Ok(tx) => tx,
                Err(_) => {
                    let legacy: LegacyTransactionV1 = bincode::deserialize(&value)
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
        let tree = self.db.open_tree(EVIDENCE_TREE)?;
        let key = evidence.key();
        let value =
            bincode::serialize(evidence).map_err(|e| StorageError::Serialize(e.to_string()))?;
        tree.insert(key, value)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_all_evidence(&self) -> Result<Vec<EquivocationEvidence>, StorageError> {
        let tree = self.db.open_tree(EVIDENCE_TREE)?;
        let mut evidence_list = Vec::new();
        for entry in tree.iter() {
            let (_key, value) = entry?;
            let evidence: EquivocationEvidence =
                bincode::deserialize(&value).map_err(|e| StorageError::Serialize(e.to_string()))?;
            evidence_list.push(evidence);
        }
        Ok(evidence_list)
    }

    /// Get set of slashed validator addresses from stored evidence
    pub fn get_slashed_addresses(
        &self,
    ) -> Result<std::collections::HashSet<Vec<u8>>, StorageError> {
        let tree = self.db.open_tree(EVIDENCE_TREE)?;
        let mut addresses = std::collections::HashSet::new();
        for entry in tree.iter() {
            let (_key, value) = entry?;
            if let Ok(evidence) = bincode::deserialize::<EquivocationEvidence>(&value) {
                addresses.insert(crate::crypto::hash::address_bytes_from_public_key(
                    &evidence.validator_public_key,
                ));
                continue;
            }

            let legacy: LegacyEquivocationEvidenceV1 =
                bincode::deserialize(&value).map_err(|e| StorageError::Serialize(e.to_string()))?;
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
        let meta = self.db.open_tree(META_TREE)?;
        match meta.get(key)? {
            Some(data) => match bincode::deserialize::<GenesisConfig>(&data) {
                Ok(value) => Ok(Some(value)),
                Err(_) => {
                    let legacy: LegacyGenesisConfigV1 = bincode::deserialize(&data)
                        .map_err(|e| StorageError::Serialize(e.to_string()))?;
                    Ok(Some(legacy.into()))
                }
            },
            None => Ok(None),
        }
    }

    pub fn put_meta<T: Serialize>(&self, key: &[u8], value: &T) -> Result<(), StorageError> {
        let meta = self.db.open_tree(META_TREE)?;
        let data = bincode::serialize(value).map_err(|e| StorageError::Serialize(e.to_string()))?;
        meta.insert(key, data)?;
        Ok(())
    }

    pub fn get_meta<T: DeserializeOwned>(&self, key: &[u8]) -> Result<Option<T>, StorageError> {
        let meta = self.db.open_tree(META_TREE)?;
        match meta.get(key)? {
            Some(data) => {
                let value: T = bincode::deserialize(&data)
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
        let tree = self.db.open_tree(EPOCH_TREE)?;
        let key = epoch.to_be_bytes();
        let value =
            bincode::serialize(snapshot).map_err(|e| StorageError::Serialize(e.to_string()))?;
        tree.insert(key, value)?;
        Ok(())
    }

    pub fn replace_epoch_snapshots(
        &self,
        snapshots: &HashMap<u64, EpochSnapshot>,
    ) -> Result<(), StorageError> {
        let tree = self.db.open_tree(EPOCH_TREE)?;
        tree.clear()?;
        let mut entries: Vec<(&u64, &EpochSnapshot)> = snapshots.iter().collect();
        entries.sort_by_key(|(epoch, _)| **epoch);
        for (epoch, snapshot) in entries {
            let key = epoch.to_be_bytes();
            let value =
                bincode::serialize(snapshot).map_err(|e| StorageError::Serialize(e.to_string()))?;
            tree.insert(key, value)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_epoch_snapshot(&self, epoch: u64) -> Result<Option<EpochSnapshot>, StorageError> {
        let tree = self.db.open_tree(EPOCH_TREE)?;
        let key = epoch.to_be_bytes();
        match tree.get(key)? {
            Some(data) => {
                let snapshot: EpochSnapshot = bincode::deserialize(&data)
                    .map_err(|e| StorageError::Serialize(e.to_string()))?;
                Ok(Some(snapshot))
            }
            None => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub fn get_all_epoch_snapshots(&self) -> Result<Vec<(u64, EpochSnapshot)>, StorageError> {
        let tree = self.db.open_tree(EPOCH_TREE)?;
        let mut snapshots = Vec::new();
        for entry in tree.iter() {
            let (key, value) = entry?;
            let epoch_bytes: [u8; 8] = key
                .as_ref()
                .try_into()
                .map_err(|_| StorageError::Serialize("invalid epoch key".to_string()))?;
            let epoch = u64::from_be_bytes(epoch_bytes);
            let snapshot: EpochSnapshot =
                bincode::deserialize(&value).map_err(|e| StorageError::Serialize(e.to_string()))?;
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
        let tree = self.db.open_tree(SNAPSHOT_MANIFEST_TREE)?;
        let key = height.to_be_bytes();
        let value =
            bincode::serialize(manifest).map_err(|e| StorageError::Serialize(e.to_string()))?;
        tree.insert(key, value)?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_latest_snapshot_manifest(&self) -> Result<Option<SnapshotManifest>, StorageError> {
        let tree = self.db.open_tree(SNAPSHOT_MANIFEST_TREE)?;
        match tree.last()? {
            Some((_key, value)) => {
                let manifest: SnapshotManifest = bincode::deserialize(&value)
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
        let tree = self.db.open_tree(SNAPSHOT_CHUNK_TREE)?;
        let mut key = height.to_be_bytes().to_vec();
        key.extend_from_slice(&(chunk.index as u64).to_be_bytes());
        let value =
            bincode::serialize(chunk).map_err(|e| StorageError::Serialize(e.to_string()))?;
        tree.insert(key, value)?;
        Ok(())
    }

    pub fn get_snapshot_chunks(&self, height: u64) -> Result<Vec<StateChunk>, StorageError> {
        let tree = self.db.open_tree(SNAPSHOT_CHUNK_TREE)?;
        let prefix = height.to_be_bytes();
        let mut chunks = Vec::new();
        for entry in tree.scan_prefix(prefix) {
            let (_key, value) = entry?;
            let chunk: StateChunk =
                bincode::deserialize(&value).map_err(|e| StorageError::Serialize(e.to_string()))?;
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
        let tree = self.db.open_tree(SNAPSHOT_CHUNK_TREE)?;
        let mut key = height.to_be_bytes().to_vec();
        key.extend_from_slice(&(index as u64).to_be_bytes());
        match tree.get(key)? {
            Some(data) => {
                let chunk: StateChunk = bincode::deserialize(&data)
                    .map_err(|e| StorageError::Serialize(e.to_string()))?;
                Ok(Some(chunk))
            }
            None => Ok(None),
        }
    }

    pub fn flush(&self) -> Result<(), StorageError> {
        self.db.flush()?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sled error: {0}")]
    Sled(#[from] sled::Error),
    #[error("serialization error: {0}")]
    Serialize(String),
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
}
