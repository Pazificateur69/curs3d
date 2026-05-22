//! Merkle-inclusion proofs for accounts and contract storage, plus the
//! pure generation + verification functions.
//!
//! The two structs are the on-the-wire shapes that the HTTP API
//! returns (`AccountProof`, `StorageProof`); the generation /
//! verification logic was lifted out of `chain.rs` in #29 alongside
//! `state_root` so all proof code lives in one place.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::chain::AccountState;
use crate::core::state_root;
use crate::crypto::hash;
use crate::vm::state::ContractState;

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

/// Generate an inclusion proof for `address` against the v5 linear-Merkle
/// state root. Returns `None` if the account isn't in `accounts`.
pub fn generate_account_proof(
    address: &[u8],
    accounts: &HashMap<Vec<u8>, AccountState>,
    contracts: &HashMap<Vec<u8>, ContractState>,
) -> Option<AccountProof> {
    let state = accounts.get(address)?.clone();
    let mut account_entries: Vec<(&Vec<u8>, &AccountState)> = accounts.iter().collect();
    account_entries.sort_by_key(|(a, _)| *a);
    let account_index = account_entries
        .iter()
        .position(|(entry_address, _)| entry_address.as_slice() == address)?;
    let leaf_hash = state_root::account_leaf_hash(address, &state);
    let leaves = state_root::state_leaf_hashes(accounts, contracts);

    Some(AccountProof {
        address: address.to_vec(),
        state,
        leaf_index: account_index,
        leaf_hash,
        proof: hash::merkle_proof(&leaves, account_index),
        state_root: state_root::compute_full(accounts, contracts),
    })
}

/// Verify an `AccountProof`. Pure — no chain state required.
pub fn verify_account_proof(proof: &AccountProof) -> bool {
    let expected_leaf = state_root::account_leaf_hash(&proof.address, &proof.state);
    expected_leaf == proof.leaf_hash
        && hash::verify_merkle_proof(
            &proof.leaf_hash,
            &proof.proof,
            proof.leaf_index,
            &proof.state_root,
        )
}

/// Generate a storage-slot proof for `(contract_address, key)`. Returns
/// `None` if the contract or the slot isn't present. The proof binds
/// both the storage leaf and the containing contract leaf to the v5
/// state root, so a verifier holding only `state_root` can validate
/// `(key, value)` without re-running the contract.
pub fn generate_storage_proof(
    contract_address: &[u8],
    key: &[u8],
    accounts: &HashMap<Vec<u8>, AccountState>,
    contracts: &HashMap<Vec<u8>, ContractState>,
) -> Option<StorageProof> {
    let contract = contracts.get(contract_address)?;
    let value = contract.storage.get(key)?.clone();

    let mut account_entries: Vec<(&Vec<u8>, &AccountState)> = accounts.iter().collect();
    account_entries.sort_by_key(|(a, _)| *a);
    let account_count = account_entries.len();

    let mut contract_entries: Vec<(&Vec<u8>, &ContractState)> = contracts.iter().collect();
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
    let contract_leaf_hash = state_root::contract_leaf_hash(contract_address, contract);
    let state_leaves = state_root::state_leaf_hashes(accounts, contracts);
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
        state_root: state_root::compute_full(accounts, contracts),
    })
}

/// Verify a `StorageProof`. Pure — no chain state required.
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
