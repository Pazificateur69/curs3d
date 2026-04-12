use serde::{Deserialize, Serialize};

use crate::core::transaction::Transaction;
use crate::crypto::hash;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    pub timestamp: i64,
    pub prev_hash: Vec<u8>,
    pub merkle_root: Vec<u8>,
    pub validator: Vec<u8>,
    pub nonce: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub hash: Vec<u8>,
}

impl Block {
    pub fn new(
        height: u64,
        prev_hash: Vec<u8>,
        transactions: Vec<Transaction>,
        validator: Vec<u8>,
    ) -> Self {
        let tx_hashes: Vec<Vec<u8>> = transactions.iter().map(|tx| tx.hash()).collect();
        let merkle_root = hash::merkle_root(&tx_hashes);

        let header = BlockHeader {
            version: 1,
            height,
            timestamp: chrono::Utc::now().timestamp(),
            prev_hash,
            merkle_root,
            validator,
            nonce: 0,
        };

        let hash = Self::compute_hash(&header);

        Block {
            header,
            transactions,
            hash,
        }
    }

    pub fn genesis() -> Self {
        let coinbase = Transaction::coinbase(vec![0; 32], 0);
        let tx_hashes = vec![coinbase.hash()];
        let merkle_root = hash::merkle_root(&tx_hashes);

        let header = BlockHeader {
            version: 1,
            height: 0,
            timestamp: 1700000000,
            prev_hash: vec![0; 32],
            merkle_root,
            validator: vec![0; 32],
            nonce: 0,
        };

        let hash = Self::compute_hash(&header);

        Block {
            header,
            transactions: vec![coinbase],
            hash,
        }
    }

    fn compute_hash(header: &BlockHeader) -> Vec<u8> {
        let serialized = bincode::serialize(header).expect("failed to serialize header");
        hash::double_hash(&serialized)
    }

    pub fn hash_hex(&self) -> String {
        hex::encode(&self.hash)
    }

    pub fn verify_merkle_root(&self) -> bool {
        let tx_hashes: Vec<Vec<u8>> = self.transactions.iter().map(|tx| tx.hash()).collect();
        let computed = hash::merkle_root(&tx_hashes);
        computed == self.header.merkle_root
    }

    pub fn total_fees(&self) -> u64 {
        self.transactions.iter().map(|tx| tx.fee).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_block() {
        let genesis = Block::genesis();
        assert_eq!(genesis.header.height, 0);
        assert!(genesis.verify_merkle_root());
    }

    #[test]
    fn test_new_block() {
        let genesis = Block::genesis();
        let block = Block::new(
            1,
            genesis.hash.clone(),
            vec![Transaction::coinbase(vec![1; 32], 50)],
            vec![1; 32],
        );
        assert_eq!(block.header.height, 1);
        assert_eq!(block.header.prev_hash, genesis.hash);
        assert!(block.verify_merkle_root());
    }
}
