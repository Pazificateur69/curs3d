use serde::{Deserialize, Serialize};

use crate::core::block::{Block, BlockHeader};
use crate::core::state_proof::{AccountProof, StorageProof};
use crate::crypto::{dilithium, hash};

// ─── Light Client ───────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedHeader {
    pub chain_id: String,
    pub header: BlockHeader,
    pub block_hash: Vec<u8>,
    pub signature: Option<dilithium::Signature>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LightClient {
    pub chain_id: String,
    pub genesis_hash: Vec<u8>,
    pub headers: Vec<SignedHeader>,
    pub finalized_height: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LightClientError {
    #[error("invalid block header hash")]
    InvalidHash,
    #[error("invalid chain id")]
    InvalidChainId,
    #[error("invalid previous hash link")]
    InvalidPrevHash,
    #[error("invalid block height sequence")]
    InvalidHeight,
    #[error("invalid validator signature")]
    InvalidSignature,
    #[error("missing parent header")]
    MissingParent,
    #[error("invalid merkle proof")]
    InvalidProof,
    #[error("empty header chain")]
    EmptyHeaders,
}

impl LightClient {
    pub fn new(chain_id: String, genesis_hash: Vec<u8>) -> Self {
        Self {
            chain_id,
            genesis_hash,
            headers: Vec::new(),
            finalized_height: 0,
        }
    }

    pub fn height(&self) -> u64 {
        self.headers.last().map(|h| h.header.height).unwrap_or(0)
    }

    /// Verify and append a chain of headers.
    pub fn sync_headers(&mut self, headers: Vec<SignedHeader>) -> Result<u64, LightClientError> {
        if headers.is_empty() {
            return Err(LightClientError::EmptyHeaders);
        }

        for signed_header in &headers {
            self.verify_header(signed_header)?;
            self.headers.push(signed_header.clone());
        }

        Ok(self.height())
    }

    /// Verify a single header against the current chain state.
    fn verify_header(&self, signed_header: &SignedHeader) -> Result<(), LightClientError> {
        if signed_header.chain_id != self.chain_id {
            return Err(LightClientError::InvalidChainId);
        }

        let header = &signed_header.header;

        // Check height sequence
        let expected_height = if self.headers.is_empty() {
            0
        } else {
            self.headers.last().unwrap().header.height + 1
        };

        if header.height != expected_height {
            return Err(LightClientError::InvalidHeight);
        }

        let computed_hash = Block::compute_hash(header);
        if computed_hash != signed_header.block_hash {
            return Err(LightClientError::InvalidHash);
        }

        if header.height == 0 {
            if signed_header.block_hash != self.genesis_hash || signed_header.signature.is_some() {
                return Err(LightClientError::InvalidHash);
            }
            return Ok(());
        }

        let Some(prev) = self.headers.last() else {
            return Err(LightClientError::MissingParent);
        };
        if header.prev_hash != prev.block_hash {
            return Err(LightClientError::InvalidPrevHash);
        }

        let signature = signed_header
            .signature
            .as_ref()
            .ok_or(LightClientError::InvalidSignature)?;
        if !Self::verify_block_signature(header, &signed_header.block_hash, signature) {
            return Err(LightClientError::InvalidSignature);
        }

        Ok(())
    }

    /// Verify a Merkle account proof against a known state root.
    pub fn verify_account_proof(proof: &AccountProof) -> bool {
        let encoded = bincode::serialize(&(&proof.address, &proof.state)).unwrap_or_default();
        let expected_leaf = hash::sha3_hash(&encoded);

        expected_leaf == proof.leaf_hash
            && hash::verify_merkle_proof(
                &proof.leaf_hash,
                &proof.proof,
                proof.leaf_index,
                &proof.state_root,
            )
    }

    /// Verify a Merkle storage proof against a known state root.
    pub fn verify_storage_proof(proof: &StorageProof) -> bool {
        let storage_leaf = {
            let encoded = bincode::serialize(&(&proof.key, &proof.value)).unwrap_or_default();
            hash::sha3_hash(&encoded)
        };
        if storage_leaf != proof.storage_leaf_hash {
            return false;
        }

        // First verify the storage slot proof against the contract's storage root
        let storage_valid = hash::verify_merkle_proof(
            &proof.storage_leaf_hash,
            &proof.storage_proof,
            proof.storage_leaf_index,
            &proof.storage_root,
        );

        // Then verify the contract proof against the global state root
        let contract_valid = hash::verify_merkle_proof(
            &proof.contract_leaf_hash,
            &proof.contract_proof,
            proof.contract_leaf_index,
            &proof.state_root,
        );

        let contract_leaf = {
            let encoded = bincode::serialize(&(
                &proof.contract_address,
                &proof.contract_code_hash,
                &proof.contract_owner,
                &proof.storage_root,
            ))
            .unwrap_or_default();
            hash::sha3_hash(&encoded)
        };

        storage_valid && contract_leaf == proof.contract_leaf_hash && contract_valid
    }

    /// Get the state root at a given height.
    pub fn state_root_at(&self, height: u64) -> Option<&Vec<u8>> {
        self.headers
            .get(height as usize)
            .map(|signed| &signed.header.state_root)
    }

    /// Verify that a validator signed a block at a given height.
    pub fn verify_block_signature(
        header: &BlockHeader,
        block_hash: &[u8],
        signature: &dilithium::Signature,
    ) -> bool {
        dilithium::verify(block_hash, signature, &header.validator_public_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_light_client_new() {
        let lc = LightClient::new("test-chain".to_string(), vec![0u8; 32]);
        assert_eq!(lc.chain_id, "test-chain");
        assert_eq!(lc.height(), 0);
        assert_eq!(lc.finalized_height, 0);
    }

    #[test]
    fn test_verify_account_proof_valid() {
        let address = vec![1u8; 20];
        let state = crate::core::chain::AccountState::default();
        let leaf_hash = hash::sha3_hash(&bincode::serialize(&(&address, &state)).unwrap());
        let leaves = vec![leaf_hash.clone()];
        let root = hash::merkle_root(&leaves);
        let proof_path = hash::merkle_proof(&leaves, 0);

        let account_proof = AccountProof {
            address,
            state,
            leaf_index: 0,
            leaf_hash: leaf_hash.clone(),
            proof: proof_path,
            state_root: root,
        };

        assert!(LightClient::verify_account_proof(&account_proof));
    }

    #[test]
    fn test_verify_account_proof_invalid() {
        let leaf_hash = hash::sha3_hash(b"test");
        let fake_root = hash::sha3_hash(b"fake root");

        let account_proof = AccountProof {
            address: vec![1u8; 20],
            state: crate::core::chain::AccountState::default(),
            leaf_index: 0,
            leaf_hash,
            proof: vec![],
            state_root: fake_root,
        };

        // Empty proof with mismatched root should fail
        assert!(!LightClient::verify_account_proof(&account_proof));
    }

    #[test]
    fn test_sync_empty_headers_rejected() {
        let mut lc = LightClient::new("test".to_string(), vec![]);
        assert_eq!(lc.sync_headers(vec![]), Err(LightClientError::EmptyHeaders));
    }

    #[test]
    fn test_sync_headers_validates_genesis_anchor_and_signature() {
        let validator = crate::crypto::dilithium::KeyPair::generate();
        let genesis = crate::core::block::Block::genesis_with_state_root(
            hash::sha3_hash(b"state-root"),
            "test-chain",
            0,
        );
        let block = crate::core::block::Block::new(
            1,
            1,
            genesis.hash.clone(),
            hash::sha3_hash(b"state-root-2"),
            0,
            0,
            vec![crate::core::transaction::Transaction::coinbase(
                "test-chain",
                vec![0; crate::crypto::hash::ADDRESS_LEN],
                0,
            )],
            &validator,
        );

        let mut lc = LightClient::new("test-chain".to_string(), genesis.hash.clone());
        lc.sync_headers(vec![
            SignedHeader {
                chain_id: "test-chain".to_string(),
                header: genesis.header.clone(),
                block_hash: genesis.hash.clone(),
                signature: None,
            },
            SignedHeader {
                chain_id: "test-chain".to_string(),
                header: block.header.clone(),
                block_hash: block.hash.clone(),
                signature: block.signature.clone(),
            },
        ])
        .unwrap();

        assert_eq!(lc.height(), 1);
    }
}
