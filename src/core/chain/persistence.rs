//! Persistence helpers — every method that talks to `Storage` or
//! enqueues onto the async `PersistenceHandle`. Extracted from
//! `chain/mod.rs` in #29.
//!
//! Two paths:
//! - **Sync** — write straight to redb under the chain lock. Used by
//!   tests and the CLI tool path; never the live node.
//! - **Async** — enqueue onto the `PersistenceHandle` worker thread.
//!   Used by every production node so storage I/O can never block the
//!   consensus / RPC / gossipsub critical path. This is what kept the
//!   live testnet alive on 2026-05-06 after sled 0.34's IO-buffer
//!   deadlock; the post-redb era inherits the same architecture for
//!   the same reason.

use super::{
    Blockchain, CHAIN_CONFIG_KEY, ChainError, PersistJob, PersistedChainState, PersistenceMode,
};
use crate::consensus::EquivocationEvidence;
use crate::core::block::Block;
use crate::crypto::hash;
use crate::storage::Storage;

impl Blockchain {
    pub(super) fn persist_full_state(&self) -> Result<(), ChainError> {
        if self.storage.is_none() {
            return Ok(());
        }

        let started = std::time::Instant::now();
        let state = PersistedChainState::from_chain(self);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        // Surface clone latency: this work happens under chain.lock(), so a
        // slow clone directly translates into consensus / RPC / gossipsub
        // jitter. Above 200ms is a yellow flag, above 500ms is red.
        if elapsed_ms > 200 {
            tracing::warn!(
                target: "storage",
                event = "persisted_state_clone_slow",
                elapsed_ms,
                blocks = state.blocks.len(),
                accounts = state.accounts.len(),
                contracts = state.contracts.len(),
            );
        } else {
            tracing::debug!(
                target: "storage",
                event = "persisted_state_clone",
                elapsed_ms,
                blocks = state.blocks.len(),
            );
        }

        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    Self::write_full_state_to_storage(storage, &state)?;
                }
            }
            PersistenceMode::Async(handle) => {
                handle.enqueue_full_state(Box::new(state));
            }
        }
        Ok(())
    }

    pub(super) fn write_full_state_to_storage(
        storage: &Storage,
        state: &PersistedChainState,
    ) -> Result<(), ChainError> {
        storage.put_meta(CHAIN_CONFIG_KEY, &state.genesis_config)?;
        storage.put_meta(b"finalized_height", &state.finalized_height)?;
        storage.put_meta(
            crate::storage::SCHEMA_VERSION_KEY,
            &crate::storage::CURRENT_SCHEMA_VERSION,
        )?;
        storage.put_meta(crate::storage::TOKEN_REGISTRY_KEY, &state.token_registry)?;
        storage.put_meta(crate::storage::GOVERNANCE_STATE_KEY, &state.governance)?;
        storage.replace_blocks(&state.blocks)?;
        storage.replace_accounts(&state.accounts)?;
        storage.replace_contracts(&state.contracts)?;
        storage.replace_receipts(&state.receipts)?;
        storage.replace_epoch_snapshots(&state.epoch_snapshots)?;
        storage.replace_pending_transactions(&state.pending_transactions)?;
        storage.flush()?;
        Ok(())
    }

    pub(super) fn persist_pending_transactions(&self) -> Result<(), ChainError> {
        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    storage.replace_pending_transactions(&self.pending_transactions)?;
                    storage.flush()?;
                }
            }
            PersistenceMode::Async(handle) => {
                handle.try_enqueue(
                    PersistJob::PendingTransactions(self.pending_transactions.clone()),
                    "pending_transactions",
                );
            }
        }
        Ok(())
    }

    pub(super) fn persist_added_block(&self, block: &Block) -> Result<(), ChainError> {
        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    storage.put_block(block)?;
                }
            }
            PersistenceMode::Async(_) => {
                // Full state snapshots at epoch boundaries persist blocks,
                // state, receipts, and pending txs together. Avoiding one
                // sled write per block is the point of async live mode.
            }
        }
        Ok(())
    }

    pub(super) fn persist_finalized_height(&self, height: u64) {
        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    let _ = storage.put_meta(b"finalized_height", &height);
                    let _ = storage.flush();
                }
            }
            PersistenceMode::Async(handle) => {
                handle.try_enqueue(PersistJob::FinalizedHeight(height), "finalized_height");
            }
        }
    }

    pub(super) fn persist_equivocation(&self, evidence: &EquivocationEvidence) {
        let address = hash::address_bytes_from_public_key(&evidence.validator_public_key);
        let account = self.accounts.get(&address).cloned();
        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    let _ = storage.put_evidence(evidence);
                    if let Some(ref account) = account {
                        let _ = storage.put_account(&address, account);
                    }
                    let _ = storage.flush();
                }
            }
            PersistenceMode::Async(handle) => {
                handle.try_enqueue(
                    PersistJob::EquivocationEvidence {
                        evidence: Box::new(evidence.clone()),
                        address,
                        account,
                    },
                    "equivocation_evidence",
                );
            }
        }
    }
}
