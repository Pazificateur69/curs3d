use serde::{Deserialize, Serialize};

use crate::core::transaction::Transaction;
use crate::crypto::dilithium::{self, KeyPair, Signature};
use crate::crypto::hash;

pub const GENESIS_TIMESTAMP: i64 = 1_700_000_000;
pub const EMPTY_STATE_ROOT_SEED: &[u8] = b"curs3d-empty-state";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    pub timestamp: i64,
    pub prev_hash: Vec<u8>,
    pub merkle_root: Vec<u8>,
    pub state_root: Vec<u8>,
    #[serde(default)]
    pub gas_used: u64,
    #[serde(default)]
    pub base_fee_per_gas: u64,
    pub validator_public_key: Vec<u8>,
    pub nonce: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub hash: Vec<u8>,
    pub signature: Option<Signature>,
}

impl Block {
    pub fn new(
        version: u32,
        height: u64,
        prev_hash: Vec<u8>,
        state_root: Vec<u8>,
        gas_used: u64,
        base_fee_per_gas: u64,
        transactions: Vec<Transaction>,
        validator_keypair: &KeyPair,
    ) -> Self {
        let tx_hashes: Vec<Vec<u8>> = transactions.iter().map(|tx| tx.hash()).collect();
        let merkle_root = hash::merkle_root(&tx_hashes);

        let header = BlockHeader {
            version,
            height,
            timestamp: chrono::Utc::now().timestamp(),
            prev_hash,
            merkle_root,
            state_root,
            gas_used,
            base_fee_per_gas,
            validator_public_key: validator_keypair.public_key.clone(),
            nonce: 0,
        };

        let hash = Self::compute_hash(&header);
        let signature = Some(validator_keypair.sign(&hash));

        Block {
            header,
            transactions,
            hash,
            signature,
        }
    }

    pub fn genesis() -> Self {
        Self::genesis_with_state_root(
            hash::sha3_hash(EMPTY_STATE_ROOT_SEED),
            "curs3d-devnet",
            0,
        )
    }

    pub fn genesis_with_state_root(
        state_root: Vec<u8>,
        chain_id: &str,
        initial_base_fee_per_gas: u64,
    ) -> Self {
        let coinbase = Transaction::coinbase_with_timestamp(
            chain_id,
            vec![0; hash::ADDRESS_LEN],
            0,
            GENESIS_TIMESTAMP,
        );
        let tx_hashes = vec![coinbase.hash()];
        let merkle_root = hash::merkle_root(&tx_hashes);

        let header = BlockHeader {
            version: 1,
            height: 0,
            timestamp: GENESIS_TIMESTAMP,
            prev_hash: vec![0; 32],
            merkle_root,
            state_root,
            gas_used: 0,
            base_fee_per_gas: initial_base_fee_per_gas,
            validator_public_key: Vec::new(),
            nonce: 0,
        };

        let hash = Self::compute_hash(&header);

        Block {
            header,
            transactions: vec![coinbase],
            hash,
            signature: None,
        }
    }

    pub fn compute_hash(header: &BlockHeader) -> Vec<u8> {
        let serialized = bincode::serialize(header).expect("failed to serialize header");
        hash::double_hash(&serialized)
    }

    pub fn hash_hex(&self) -> String {
        hex::encode(&self.hash)
    }

    pub fn verify_hash(&self) -> bool {
        self.hash == Self::compute_hash(&self.header)
    }

    pub fn verify_signature(&self) -> bool {
        if self.header.height == 0 {
            return self.signature.is_none();
        }

        match &self.signature {
            Some(signature) => {
                dilithium::verify(&self.hash, signature, &self.header.validator_public_key)
            }
            None => false,
        }
    }

    pub fn verify_merkle_root(&self) -> bool {
        let tx_hashes: Vec<Vec<u8>> = self.transactions.iter().map(|tx| tx.hash()).collect();
        let computed = hash::merkle_root(&tx_hashes);
        computed == self.header.merkle_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_block() {
        let genesis = Block::genesis();
        assert_eq!(genesis.header.height, 0);
        assert!(genesis.verify_hash());
        assert!(genesis.verify_merkle_root());
        assert!(genesis.verify_signature());
    }

    #[test]
    fn test_new_block() {
        let genesis = Block::genesis();
        let validator = KeyPair::generate();
        let block = Block::new(
            1,
            1,
            genesis.hash.clone(),
            hash::sha3_hash(b"state"),
            21_000,
            1,
            vec![Transaction::coinbase("curs3d-devnet", vec![1; hash::ADDRESS_LEN], 50)],
            &validator,
        );
        assert_eq!(block.header.height, 1);
        assert_eq!(block.header.prev_hash, genesis.hash);
        assert!(block.verify_hash());
        assert!(block.verify_merkle_root());
        assert!(block.verify_signature());
    }
}
