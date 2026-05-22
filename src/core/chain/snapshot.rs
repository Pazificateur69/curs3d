//! State-sync snapshots: build, serialize-into-chunks, validate, and
//! apply to local state.
//!
//! Lifted out of `chain/mod.rs` in #29 as a child module so it can
//! reach `Blockchain`'s private fields (`finality_tracker`,
//! `epoch_snapshots`, `block_tree`, etc.) directly. The snapshot path
//! is intricate but self-contained — it leans on
//! `replay_state_to_canonical_height` and `replace_all_blocks` /
//! `rebuild_receipt_indexes` / `persist_full_state` from `mod.rs`, all
//! of which are reachable across `impl Blockchain` blocks via plain
//! method calls.

use std::collections::HashMap;

use super::{AccountState, Blockchain, ChainError};
use crate::consensus::FinalityTracker;
use crate::core::blocktree::BlockTree;
use crate::core::checkpoints;
use crate::crypto::hash;
use crate::vm::state::ContractState;

impl Blockchain {
    /// Create a state sync snapshot from the current chain state.
    pub fn create_snapshot(&self) -> Result<crate::storage::SnapshotManifest, ChainError> {
        let snapshot_height = if self.finality_tracker.finalized_height > 0 {
            self.finality_tracker.finalized_height.min(self.height())
        } else {
            self.height()
        };
        let snapshot_hash = self
            .block_at_height(snapshot_height)
            .map(|block| block.hash)
            .ok_or_else(|| ChainError::SnapshotError("snapshot height missing".to_string()))?;
        let (snapshot_accounts, snapshot_contracts, snapshot_receipts, _, _) =
            self.replay_state_to_canonical_height(snapshot_height)?;
        let mut accounts: Vec<(Vec<u8>, AccountState)> = snapshot_accounts
            .iter()
            .map(|(address, state)| (address.clone(), state.clone()))
            .collect();
        accounts.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut contracts: Vec<(Vec<u8>, ContractState)> = snapshot_contracts
            .iter()
            .map(|(address, state)| (address.clone(), state.clone()))
            .collect();
        contracts.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut receipts: Vec<(Vec<u8>, crate::core::receipt::Receipt)> = snapshot_receipts
            .iter()
            .map(|(tx_hash, receipt)| (tx_hash.clone(), receipt.clone()))
            .collect();
        receipts.sort_by(|(a, _), (b, _)| a.cmp(b));

        let mut epoch_snapshots: Vec<(u64, crate::consensus::EpochSnapshot)> = self
            .epoch_snapshots
            .iter()
            .filter(|(epoch, _)| **epoch <= self.epoch_for_height(snapshot_height))
            .map(|(epoch, snapshot)| (*epoch, snapshot.clone()))
            .collect();
        epoch_snapshots.sort_by_key(|(epoch, _)| *epoch);

        let snapshot_state = crate::storage::SnapshotState {
            blocks: self
                .iter_blocks()
                .take(snapshot_height as usize + 1)
                .collect(),
            accounts,
            contracts,
            receipts,
            pending_transactions: Vec::new(),
            slashed_validators: self.slashed_validators.iter().cloned().collect(),
            epoch_snapshots,
            finalized_height: snapshot_height,
            finalized_hash: snapshot_hash.clone(),
        };

        let snapshot_bytes = bincode::serialize(&snapshot_state)
            .map_err(|e| ChainError::SnapshotError(e.to_string()))?;

        let chunk_size = 256 * 1024;
        let mut chunks = Vec::new();
        let mut chunk_hashes = Vec::new();
        for (index, data) in snapshot_bytes.chunks(chunk_size).enumerate() {
            let chunk_hash = hash::sha3_hash(data);
            chunk_hashes.push(chunk_hash.clone());
            chunks.push((index, data.to_vec(), chunk_hash));
        }
        let chunk_root = hash::merkle_root(&chunk_hashes);
        let mut chunks: Vec<crate::storage::StateChunk> = chunks
            .into_iter()
            .map(|(index, data, chunk_hash)| crate::storage::StateChunk {
                index,
                data,
                hash: chunk_hash,
                proof: hash::merkle_proof(&chunk_hashes, index),
            })
            .collect();
        if chunks.is_empty() {
            chunks.push(crate::storage::StateChunk {
                index: 0,
                data: Vec::new(),
                hash: hash::sha3_hash(&[]),
                proof: Vec::new(),
            });
        }

        let epoch = self.epoch_for_height(snapshot_height);
        let state_root = Self::compute_state_root_full(&snapshot_accounts, &snapshot_contracts);
        let manifest = crate::storage::SnapshotManifest {
            height: snapshot_height,
            epoch,
            chain_id: self.chain_id().to_string(),
            genesis_hash: self.genesis_hash().to_vec(),
            latest_hash: snapshot_hash.clone(),
            tip_height: self.height(),
            tip_hash: self.latest_hash().to_vec(),
            finalized_height: snapshot_height,
            finalized_hash: snapshot_hash,
            state_root,
            chunk_root,
            chunk_count: chunks.len(),
            chunk_hashes,
        };

        // Persist chunks to storage. Failures here are visible (the
        // previous `let _ =` swallowed them, which produced a
        // half-committed snapshot where the manifest claimed N chunks
        // but the chunk table had fewer or none — the live testnet hit
        // this on 2026-05-20 with the symptom of manifests arriving at
        // node3 but never the chunks). Returning Err here is preferable
        // to silently shipping a broken snapshot.
        if let Some(ref storage) = self.storage {
            for chunk in &chunks {
                storage
                    .put_snapshot_chunk(snapshot_height, chunk, Some(chunks.len()))
                    .map_err(|e| {
                        ChainError::SnapshotError(format!(
                            "put_snapshot_chunk(height={snapshot_height}, index={}) failed: {e}",
                            chunk.index
                        ))
                    })?;
            }
            storage
                .put_snapshot_manifest(snapshot_height, &manifest)
                .map_err(|e| {
                    ChainError::SnapshotError(format!(
                        "put_snapshot_manifest(height={snapshot_height}) failed: {e}"
                    ))
                })?;
        }

        Ok(manifest)
    }

    pub fn get_snapshot_chunks(
        &self,
        height: u64,
    ) -> Result<Vec<crate::storage::StateChunk>, ChainError> {
        if let Some(ref storage) = self.storage {
            return storage
                .get_snapshot_chunks(height)
                .map_err(ChainError::from);
        }
        Err(ChainError::SnapshotError(
            "snapshot chunks unavailable without storage backend".to_string(),
        ))
    }

    fn decode_snapshot_state(
        manifest: &crate::storage::SnapshotManifest,
        chunks: &[crate::storage::StateChunk],
    ) -> Result<crate::storage::SnapshotState, ChainError> {
        if chunks.len() != manifest.chunk_count {
            return Err(ChainError::SnapshotError(format!(
                "expected {} chunks, got {}",
                manifest.chunk_count,
                chunks.len()
            )));
        }
        for (i, chunk) in chunks.iter().enumerate() {
            if chunk.index != i {
                return Err(ChainError::SnapshotError(format!(
                    "unexpected chunk order: expected {}, got {}",
                    i, chunk.index
                )));
            }
            let computed_hash = hash::sha3_hash(&chunk.data);
            if i >= manifest.chunk_hashes.len() || computed_hash != manifest.chunk_hashes[i] {
                return Err(ChainError::SnapshotError(format!(
                    "chunk {} hash mismatch",
                    i
                )));
            }
            if !hash::verify_merkle_proof(&computed_hash, &chunk.proof, i, &manifest.chunk_root) {
                return Err(ChainError::SnapshotError(format!(
                    "chunk {} proof mismatch",
                    i
                )));
            }
        }

        let payload: Vec<u8> = chunks.iter().flat_map(|chunk| chunk.data.clone()).collect();
        let snapshot_state: crate::storage::SnapshotState =
            bincode::deserialize(&payload).map_err(|e| ChainError::SnapshotError(e.to_string()))?;

        let accounts: HashMap<Vec<u8>, AccountState> =
            snapshot_state.accounts.iter().cloned().collect();
        let contracts: HashMap<Vec<u8>, ContractState> =
            snapshot_state.contracts.iter().cloned().collect();
        let computed_root = Self::compute_state_root_full(&accounts, &contracts);
        if computed_root != manifest.state_root {
            return Err(ChainError::SnapshotError("state root mismatch".to_string()));
        }

        if snapshot_state.blocks.is_empty() {
            return Err(ChainError::SnapshotError(
                "snapshot does not contain canonical blocks".to_string(),
            ));
        }
        if snapshot_state.blocks[0].hash != manifest.genesis_hash {
            return Err(ChainError::SnapshotError(
                "snapshot genesis hash mismatch".to_string(),
            ));
        }
        if snapshot_state
            .blocks
            .last()
            .map(|block| block.hash.clone())
            .unwrap_or_default()
            != manifest.latest_hash
        {
            return Err(ChainError::SnapshotError(
                "snapshot latest hash mismatch".to_string(),
            ));
        }

        Ok(snapshot_state)
    }

    pub fn apply_snapshot(
        &mut self,
        manifest: &crate::storage::SnapshotManifest,
        chunks: &[crate::storage::StateChunk],
    ) -> Result<(), ChainError> {
        if manifest.chain_id != self.chain_id() {
            return Err(ChainError::SnapshotError(
                "snapshot chain_id mismatch".to_string(),
            ));
        }
        // Hardcoded-checkpoint enforcement on the snapshot. Catches the
        // case where a malicious peer offers us a fully self-consistent
        // alternative history that just happens to share our genesis.
        checkpoints::verify_snapshot_against_known(self.chain_id(), manifest)?;
        if self.finality_tracker.finalized_height >= manifest.height
            && !self.finality_tracker.finalized_hash.is_empty()
            && self.finality_tracker.finalized_height == manifest.finalized_height
            && self.finality_tracker.finalized_hash != manifest.finalized_hash
        {
            return Err(ChainError::SnapshotError(
                "snapshot finalized hash conflicts with local finalized checkpoint".to_string(),
            ));
        }
        let protected_height = self
            .finality_tracker
            .finalized_height
            .min(manifest.finalized_height);

        if let Some(local_block) = self.block_at_height(manifest.height)
            && local_block.hash != manifest.latest_hash
            && manifest.height <= protected_height
        {
            return Err(ChainError::SnapshotError(
                "snapshot latest hash conflicts with local canonical block".to_string(),
            ));
        }

        let snapshot_state = Self::decode_snapshot_state(manifest, chunks)?;

        // Defence against silent history rewrite: for every block height we
        // already have locally, the snapshot must agree on the hash. Otherwise
        // a peer could feed us a different chain history that shares the same
        // genesis (#5).
        for snapshot_block in &snapshot_state.blocks {
            if let Some(local_block) = self.block_at_height(snapshot_block.header.height)
                && local_block.hash != snapshot_block.hash
                && (snapshot_block.header.height == 0
                    || snapshot_block.header.height <= protected_height)
            {
                return Err(ChainError::SnapshotError(format!(
                    "snapshot block at protected height {} disagrees with local canonical block",
                    snapshot_block.header.height
                )));
            }
        }

        self.replace_all_blocks(snapshot_state.blocks);
        self.accounts = snapshot_state.accounts.into_iter().collect();
        self.contracts = snapshot_state.contracts.into_iter().collect();
        self.receipts = snapshot_state.receipts.into_iter().collect();
        self.rebuild_receipt_indexes();
        self.pending_transactions = snapshot_state.pending_transactions;
        self.slashed_validators = snapshot_state.slashed_validators.into_iter().collect();
        self.epoch_snapshots = snapshot_state.epoch_snapshots.into_iter().collect();

        // Genesis is guaranteed present by every Blockchain constructor;
        // genesis_block() panics on the invariant violation. Keep the
        // SnapshotError variant for forward-compat once block storage
        // becomes fallible via BlockStoreCursor.
        let genesis = self.genesis_block();
        let mut block_tree = BlockTree::from_genesis(&genesis);
        for block in self.iter_blocks().skip(1) {
            let proposer_address =
                hash::address_bytes_from_public_key(&block.header.validator_public_key);
            let proposer_stake = self
                .accounts
                .get(&proposer_address)
                .map(|account| account.staked_balance)
                .unwrap_or(0);
            let _ = block_tree.insert(block.clone(), proposer_stake);
        }
        block_tree.set_finalized(manifest.finalized_hash.clone(), manifest.finalized_height);
        self.block_tree = block_tree;
        self.finality_tracker = FinalityTracker::with_finalized(
            manifest.finalized_height,
            manifest.finalized_hash.clone(),
        );

        self.persist_full_state()?;
        tracing::info!(
            target: "audit",
            event = "snapshot_applied",
            height = manifest.height,
            finalized_height = manifest.finalized_height,
            tip_height = manifest.tip_height,
        );
        Ok(())
    }
}
