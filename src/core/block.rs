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
    #[allow(clippy::too_many_arguments)]
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
        let signable = Self::signable_block_hash(&hash);
        let signature = Some(validator_keypair.sign(&signable));

        Block {
            header,
            transactions,
            hash,
            signature,
        }
    }

    pub fn genesis() -> Self {
        Self::genesis_with_state_root(hash::sha3_hash(EMPTY_STATE_ROOT_SEED), "curs3d-devnet", 0)
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

    /// Domain-separated block hash for signing (prevents cross-layer replay)
    pub fn signable_block_hash(block_hash: &[u8]) -> Vec<u8> {
        let mut data = b"curs3d-block-sig-v1:".to_vec();
        data.extend_from_slice(block_hash);
        data
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

        let signable = Self::signable_block_hash(&self.hash);
        match &self.signature {
            Some(signature) => {
                dilithium::verify(&signable, signature, &self.header.validator_public_key)
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
            vec![Transaction::coinbase(
                "curs3d-devnet",
                vec![1; hash::ADDRESS_LEN],
                50,
            )],
            &validator,
        );
        assert_eq!(block.header.height, 1);
        assert_eq!(block.header.prev_hash, genesis.hash);
        assert!(block.verify_hash());
        assert!(block.verify_merkle_root());
        assert!(block.verify_signature());
    }

    // ─── Property-based tests (task #33) ─────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// Bincode roundtrip preserves block byte-identity. This is
        /// load-bearing for snapshot delivery and network gossip.
        #[test]
        fn prop_block_bincode_roundtrip(
            height in any::<u64>(),
            gas_used in any::<u64>(),
            base_fee in any::<u64>(),
            state_seed in proptest::collection::vec(any::<u8>(), 0..64),
        ) {
            let validator = KeyPair::generate();
            let block = Block::new(
                1,
                height,
                vec![0u8; 32],
                hash::sha3_hash(&state_seed),
                gas_used,
                base_fee,
                vec![Transaction::coinbase(
                    "proptest",
                    vec![2; hash::ADDRESS_LEN],
                    50,
                )],
                &validator,
            );
            let ser = bincode::serialize(&block).expect("ser");
            let de: Block = bincode::deserialize(&ser).expect("de");
            let re = bincode::serialize(&de).expect("re-ser");
            prop_assert_eq!(ser, re);
            prop_assert!(de.verify_hash());
            prop_assert!(de.verify_merkle_root());
            prop_assert!(de.verify_signature());
        }

        /// Mutating any header field after construction breaks the hash.
        #[test]
        fn prop_block_hash_sensitive_to_header_mutation(
            height in any::<u64>(),
            extra_gas in 1u64..1_000,
        ) {
            let validator = KeyPair::generate();
            let mut block = Block::new(
                1,
                height,
                vec![0u8; 32],
                hash::sha3_hash(b"state"),
                21_000,
                1,
                vec![Transaction::coinbase("proptest", vec![2; hash::ADDRESS_LEN], 50)],
                &validator,
            );
            prop_assert!(block.verify_hash());
            block.header.gas_used = block.header.gas_used.wrapping_add(extra_gas);
            prop_assert!(!block.verify_hash());
        }
    }
}
