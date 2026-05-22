//! State-root computation — pure module-level functions, no
//! `Blockchain` dependency.
//!
//! Two protocols co-exist:
//! - **v5** (the active baseline): linear-Merkle root over sorted leaf
//!   hashes. Cheap to compute, but inclusion proofs are O(N) and the
//!   root churns on every leaf permutation in a snapshot.
//! - **v6** (dormant, gated by `V6_HARDFORK_HEIGHT_TESTNET` in
//!   `chain.rs`): `SparseMerkleTrie` root. Fixed-depth O(log N)
//!   proofs, permutation-stable. Activated by a hardfork; the
//!   dispatcher at [`compute_at_protocol`] selects the right scheme
//!   for the protocol version the caller passes.
//!
//! First extracted from `chain.rs` as part of #29 Phase C
//! (single-module sibling extraction, ahead of the full
//! `chain/` directory split).

use crate::core::block::EMPTY_STATE_ROOT_SEED;
use crate::core::chain::AccountState;
use crate::crypto::hash;
use crate::trie::SparseMerkleTrie;
use crate::vm::state::ContractState;
use std::collections::HashMap;

/// Protocol version that activates the `SparseMerkleTrie` state-root
/// (replaces the linear-Merkle commitment used through v5). Bumping the
/// state-root scheme is a consensus-affecting change so it ships under
/// a hardfork. The dispatcher at [`compute_at_protocol`] selects
/// between v5 and v6 based on the protocol version derived from the
/// block height.
pub const V6_PROTOCOL_VERSION: u32 = 6;

/// State root over accounts only (legacy entry point — equivalent to
/// `compute_full(accounts, &empty)`).
pub fn compute(accounts: &HashMap<Vec<u8>, AccountState>) -> Vec<u8> {
    compute_full(accounts, &HashMap::new())
}

/// State root for the v5 baseline. Most call sites use this directly
/// because the chain hasn't crossed the v6 hardfork height yet; once
/// it does they should switch to [`compute_at_protocol`].
pub fn compute_full(
    accounts: &HashMap<Vec<u8>, AccountState>,
    contracts: &HashMap<Vec<u8>, ContractState>,
) -> Vec<u8> {
    compute_v5_merkle(accounts, contracts)
}

/// Dispatch state-root computation by protocol version. Versions
/// `<= 5` use the v5 linear-Merkle root (sorted leaves, one big
/// `merkle_root`); versions `>= 6` use the `SparseMerkleTrie` root
/// which gives O(log N) inclusion proofs of fixed depth and a
/// commitment that is stable under set permutations (so a snapshot
/// applied in any leaf order yields the same root).
pub fn compute_at_protocol(
    accounts: &HashMap<Vec<u8>, AccountState>,
    contracts: &HashMap<Vec<u8>, ContractState>,
    protocol_version: u32,
) -> Vec<u8> {
    if protocol_version >= V6_PROTOCOL_VERSION {
        compute_v6_smt(accounts, contracts)
    } else {
        compute_v5_merkle(accounts, contracts)
    }
}

fn compute_v5_merkle(
    accounts: &HashMap<Vec<u8>, AccountState>,
    contracts: &HashMap<Vec<u8>, ContractState>,
) -> Vec<u8> {
    if accounts.is_empty() && contracts.is_empty() {
        return hash::sha3_hash(EMPTY_STATE_ROOT_SEED);
    }

    let leaves = state_leaf_hashes(accounts, contracts);
    hash::merkle_root(&leaves)
}

/// v6 state root: insert every account + contract into a
/// `SparseMerkleTrie` keyed by `SHA3(addr_with_kind_prefix)`, then
/// return the 32-byte trie root. The kind prefix (`0x00` for
/// accounts, `0x01` for contracts) prevents a collision where a
/// contract and an account share the same address-hash slot.
///
/// The empty-state root matches `SparseMerkleTrie::root()` on an
/// empty trie, which is its own well-defined constant (not the
/// same as v5's `sha3(EMPTY_STATE_ROOT_SEED)`).
fn compute_v6_smt(
    accounts: &HashMap<Vec<u8>, AccountState>,
    contracts: &HashMap<Vec<u8>, ContractState>,
) -> Vec<u8> {
    let mut trie = SparseMerkleTrie::new();

    let mut account_entries: Vec<(&Vec<u8>, &AccountState)> = accounts.iter().collect();
    account_entries.sort_by_key(|(a, _)| *a);
    for (address, state) in account_entries {
        let mut prefixed = Vec::with_capacity(address.len() + 1);
        prefixed.push(0x00);
        prefixed.extend_from_slice(address);
        let key = hash::sha3_hash(&prefixed);
        let value = account_leaf_hash(address, state);
        trie.insert(key, value);
    }

    let mut contract_entries: Vec<(&Vec<u8>, &ContractState)> = contracts.iter().collect();
    contract_entries.sort_by_key(|(a, _)| *a);
    for (address, state) in contract_entries {
        let mut prefixed = Vec::with_capacity(address.len() + 1);
        prefixed.push(0x01);
        prefixed.extend_from_slice(address);
        let key = hash::sha3_hash(&prefixed);
        let value = contract_leaf_hash(address, state);
        trie.insert(key, value);
    }

    trie.root()
}

pub(crate) fn account_leaf_hash(address: &[u8], state: &AccountState) -> Vec<u8> {
    let encoded = bincode::serialize(&(address, state)).expect("failed to serialize account leaf");
    hash::sha3_hash(&encoded)
}

fn contract_storage_root(contract: &ContractState) -> Vec<u8> {
    let mut storage_entries: Vec<(&Vec<u8>, &Vec<u8>)> = contract.storage.iter().collect();
    storage_entries.sort_by_key(|(a, _)| *a);
    let leaves: Vec<Vec<u8>> = storage_entries
        .into_iter()
        .map(|(key, value)| {
            let encoded =
                bincode::serialize(&(key, value)).expect("failed to serialize storage leaf");
            hash::sha3_hash(&encoded)
        })
        .collect();
    hash::merkle_root(&leaves)
}

pub(crate) fn contract_leaf_hash(address: &[u8], state: &ContractState) -> Vec<u8> {
    let storage_root = contract_storage_root(state);
    let code_hash = if state.code_hash.is_empty() {
        hash::sha3_hash(&state.code)
    } else {
        state.code_hash.clone()
    };
    let encoded = bincode::serialize(&(address, code_hash, &state.owner, storage_root))
        .expect("failed to serialize contract leaf");
    hash::sha3_hash(&encoded)
}

pub(crate) fn state_leaf_hashes(
    accounts: &HashMap<Vec<u8>, AccountState>,
    contracts: &HashMap<Vec<u8>, ContractState>,
) -> Vec<Vec<u8>> {
    let mut leaves: Vec<Vec<u8>> = Vec::new();

    let mut account_entries: Vec<(&Vec<u8>, &AccountState)> = accounts.iter().collect();
    account_entries.sort_by_key(|(a, _)| *a);
    for (address, state) in account_entries {
        leaves.push(account_leaf_hash(address, state));
    }

    let mut contract_entries: Vec<(&Vec<u8>, &ContractState)> = contracts.iter().collect();
    contract_entries.sort_by_key(|(a, _)| *a);
    for (address, state) in contract_entries {
        leaves.push(contract_leaf_hash(address, state));
    }

    leaves
}
