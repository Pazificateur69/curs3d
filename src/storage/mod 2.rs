use sled::Db;
use std::path::Path;

use crate::core::block::Block;
use crate::core::chain::AccountState;
use crate::core::transaction::Transaction;

const BLOCKS_TREE: &str = "blocks";
const STATE_TREE: &str = "accounts";
const META_TREE: &str = "meta";
const PENDING_TREE: &str = "pending";
const HEIGHT_KEY: &[u8] = b"chain_height";

pub struct Storage {
    db: Db,
}

impl Storage {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, sled::Error> {
        let db = sled::open(path)?;
        Ok(Storage { db })
    }

    /// Store a block by height
    pub fn put_block(&self, block: &Block) -> Result<(), StorageError> {
        let tree = self.db.open_tree(BLOCKS_TREE)?;
        let key = block.header.height.to_be_bytes();
        let value =
            bincode::serialize(block).map_err(|e| StorageError::Serialize(e.to_string()))?;
        tree.insert(key, value)?;

        // Update height in meta
        let meta = self.db.open_tree(META_TREE)?;
        meta.insert(HEIGHT_KEY, &block.header.height.to_be_bytes())?;

        self.db.flush()?;
        Ok(())
    }

    /// Get a block by height
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

    /// Get the stored chain height
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

    /// Store account state
    pub fn put_account(&self, address: &[u8], state: &AccountState) -> Result<(), StorageError> {
        let tree = self.db.open_tree(STATE_TREE)?;
        let value =
            bincode::serialize(state).map_err(|e| StorageError::Serialize(e.to_string()))?;
        tree.insert(address, value)?;
        Ok(())
    }

    /// Get account state
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

    /// Get all accounts
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

        let tx = Transaction::coinbase(vec![1; crate::crypto::hash::ADDRESS_LEN], 50);
        storage.replace_pending_transactions(&[tx.clone()]).unwrap();

        let pending = storage.get_all_pending_transactions().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash(), tx.hash());
    }
}
