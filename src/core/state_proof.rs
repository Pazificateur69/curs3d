use serde::{Deserialize, Serialize};

use super::chain::AccountState;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountProof {
    pub address: Vec<u8>,
    pub state: AccountState,
    pub leaf_index: usize,
    pub leaf_hash: Vec<u8>,
    pub proof: Vec<Vec<u8>>,
    pub state_root: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageProof {
    pub contract_address: Vec<u8>,
    pub contract_code_hash: Vec<u8>,
    pub contract_owner: Vec<u8>,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub storage_leaf_index: usize,
    pub storage_leaf_hash: Vec<u8>,
    pub storage_proof: Vec<Vec<u8>>,
    pub storage_root: Vec<u8>,
    pub contract_leaf_index: usize,
    pub contract_leaf_hash: Vec<u8>,
    pub contract_proof: Vec<Vec<u8>>,
    pub state_root: Vec<u8>,
}
