//! Epoch snapshots, validator-set helpers, protocol-version dispatch.
//!
//! First batch extracted from `chain/mod.rs` in #29:
//! - `build_epoch_snapshot_for_accounts` (free fn, called from
//!   `from_genesis` and `snapshot_for_accounts`)
//! - `snapshot_for_accounts` — same but on `&self`
//! - `active_validator_total_stake`
//! - `create_epoch_snapshot_from_accounts` (pub, called from
//!   `add_block` epoch-boundary path)
//! - `get_epoch_snapshot` (pub, /api/metrics-adjacent helper)
//! - `protocol_version_at_height` (pub — used by validate / produce /
//!   eth_rpc to dispatch v5 vs. v6 SMT state-root)
//! - `allowed_backup_rank_for_height` (slot-leader timing window)

use std::collections::{HashMap, HashSet};

use super::{
    AccountState, Blockchain, ChainError, GenesisConfig, V6_HARDFORK_HEIGHT_TESTNET,
    V6_PROTOCOL_VERSION,
};
use crate::consensus::{EpochSnapshot, ProofOfStake, allowed_backup_rank, slot_leader_at_rank};
use crate::crypto::hash;

impl Blockchain {
    pub(super) fn build_epoch_snapshot_for_accounts(
        genesis_config: &GenesisConfig,
        epoch: u64,
        accounts: &HashMap<Vec<u8>, AccountState>,
        slashed_validators: &HashSet<Vec<u8>>,
    ) -> EpochSnapshot {
        let start_height = epoch * genesis_config.epoch_length.max(1);
        let pos = ProofOfStake::with_slashed(
            genesis_config.minimum_stake,
            slashed_validators.clone(),
            start_height,
        );
        let validators = pos.active_validators(accounts);
        let total_stake: u64 = validators.iter().map(|v| v.stake).sum();
        EpochSnapshot {
            epoch,
            start_height,
            validators,
            total_stake,
        }
    }

    pub(super) fn snapshot_for_accounts(
        &self,
        epoch: u64,
        accounts: &HashMap<Vec<u8>, AccountState>,
    ) -> EpochSnapshot {
        Self::build_epoch_snapshot_for_accounts(
            &self.genesis_config,
            epoch,
            accounts,
            &self.slashed_validators,
        )
    }

    pub(super) fn active_validator_total_stake(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
        height: u64,
    ) -> u64 {
        ProofOfStake::with_slashed(self.minimum_stake, self.slashed_validators.clone(), height)
            .active_validators(accounts)
            .into_iter()
            .map(|validator| validator.stake)
            .sum()
    }

    /// Compute and store an EpochSnapshot for the given epoch using the provided pre-epoch state.
    pub fn create_epoch_snapshot_from_accounts(
        &mut self,
        epoch: u64,
        accounts: &HashMap<Vec<u8>, AccountState>,
    ) {
        let snapshot = self.snapshot_for_accounts(epoch, accounts);
        self.epoch_snapshots.insert(epoch, snapshot);
    }

    /// Get the EpochSnapshot for a given epoch, if it exists.
    #[allow(dead_code)]
    pub fn get_epoch_snapshot(&self, epoch: u64) -> Option<&EpochSnapshot> {
        self.epoch_snapshots.get(&epoch)
    }

    /// Return the protocol version that should be active at the given height.
    ///
    /// Genesis (height 0) is always version 1 — the genesis block was minted
    /// before any consensus rule existed. From height 1 onward the baseline
    /// is version 5: ML-DSA-87 (FIPS-204) replaces NIST round-3 Dilithium-L5
    /// as the post-quantum signature scheme, so signatures produced by the
    /// browser wallet (`sdk/wasm`) verify on the node byte-for-byte. v5 is
    /// otherwise compatible with v4 (EVM dispatch, slot-leader scheduling).
    /// Genesis configs may still declare explicit `upgrades`; those override
    /// the baseline at their specified heights, in declaration order, for
    /// chains that need to model historical version transitions.
    pub fn protocol_version_at_height(&self, height: u64) -> u32 {
        // Honour explicit upgrades from genesis_config first. A bare
        // chain (no explicit upgrades) gets the v5 baseline; chains
        // with explicit upgrades replay them in order.
        let mut version = if self.genesis_config.upgrades.is_empty() {
            5u32
        } else {
            let mut v = 1u32;
            for upgrade in &self.genesis_config.upgrades {
                if upgrade.height <= height {
                    v = upgrade.version;
                }
            }
            v
        };

        // v6 SMT state-root hardfork: triggered by the
        // `V6_HARDFORK_HEIGHT_TESTNET` constant if it's set to a
        // reachable height. Currently `u64::MAX` (= dormant), so the
        // check below only fires after the constant is bumped and a
        // coordinated rollout reaches the chosen height. The
        // `>=` reads as absurd while the constant is u64::MAX, but
        // becomes meaningful the moment we activate v6 (the constant
        // will be bumped to a real height during the v6 rollout).
        #[allow(clippy::absurd_extreme_comparisons)]
        if height >= V6_HARDFORK_HEIGHT_TESTNET && version < V6_PROTOCOL_VERSION {
            version = V6_PROTOCOL_VERSION;
        }

        version
    }

    pub(super) fn allowed_backup_rank_for_height(
        block_height: u64,
        parent_timestamp: i64,
        block_timestamp: i64,
        snapshot_size: usize,
    ) -> u32 {
        // Genesis often has a timestamp of 0 in dev/testnet configs. If we fed
        // that into the normal timeout calculation, every backup rank would be
        // admissible for block #1 and freshly started validators could all
        // create incompatible first blocks. Make the first post-genesis block
        // primary-only; backup view-change starts from block #2 onward, once
        // there is a real parent timestamp shared by the network.
        if block_height <= 1 {
            return 0;
        }
        allowed_backup_rank(parent_timestamp, block_timestamp, snapshot_size)
    }

    /// Return the snapshot to use for slot-leader selection at the given height.
    ///
    /// Prefers a frozen snapshot at the matching epoch. Falls back to a freshly
    /// computed snapshot when we're crossing into a new epoch and the snapshot
    /// hasn't been persisted yet (boundary block in `add_block`). The genesis
    /// snapshot stored in `from_genesis` is built at `start_height = 0` and is
    /// therefore empty (genesis validators activate at height 1); for any
    /// post-genesis lookup with an empty cached snapshot we re-derive a fresh
    /// snapshot from the current accounts so the slot-leader function has a
    /// non-empty validator set.
    pub(super) fn snapshot_for_height(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
        epoch_snapshots: &HashMap<u64, EpochSnapshot>,
        block_height: u64,
    ) -> Option<EpochSnapshot> {
        let epoch = block_height / self.epoch_length.max(1);
        if let Some(snapshot) = epoch_snapshots.get(&epoch)
            && !snapshot.validators.is_empty()
        {
            return Some(snapshot.clone());
        }
        // Cached but empty (genesis-epoch corner case) — fall through to
        // live derivation below.
        if block_height > 0 {
            // Build a fresh snapshot keyed on the *block height* so genesis
            // validators (active_from_height = 1) actually pass the filter.
            let pos = ProofOfStake::with_slashed(
                self.minimum_stake,
                self.slashed_validators.clone(),
                block_height,
            );
            let validators = pos.active_validators(accounts);
            if validators.is_empty() {
                return None;
            }
            let total_stake: u64 = validators.iter().map(|v| v.stake).sum();
            return Some(EpochSnapshot {
                epoch,
                start_height: block_height,
                validators,
                total_stake,
            });
        }
        None
    }

    pub(super) fn ensure_validator_is_authorized_for_accounts_at_rank(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
        epoch_snapshots: &HashMap<u64, EpochSnapshot>,
        validator_public_key: &[u8],
        block_height: u64,
        prev_hash: &[u8],
        allowed_rank: u32,
    ) -> Result<(), ChainError> {
        let proposer_address = hash::address_bytes_from_public_key(validator_public_key);

        if let Some(snapshot) = self.snapshot_for_height(accounts, epoch_snapshots, block_height) {
            // Empty snapshot → pre-stake bootstrap, anyone with a public key may
            // propose. Same liberal default that the legacy path applied.
            if snapshot.validators.is_empty() {
                return Ok(());
            }
            for rank in 0..=allowed_rank {
                if let Some(addr) = slot_leader_at_rank(&snapshot, block_height, prev_hash, rank)
                    && addr == proposer_address
                {
                    return Ok(());
                }
            }
            return Err(ChainError::WrongProposer {
                height: block_height,
                allowed_rank,
            });
        }

        // No snapshot anywhere (very early bootstrap) — fall back to live POS,
        // which loops over current accounts. This branch only fires for chains
        // running without epoch snapshots yet, which since the slot-leader
        // hard-fork should be rare.
        let pos = ProofOfStake::with_slashed(
            self.minimum_stake,
            self.slashed_validators.clone(),
            block_height,
        );
        match pos.select_validator(accounts, block_height, prev_hash) {
            Some(expected) if expected.public_key == validator_public_key => Ok(()),
            Some(_) => Err(ChainError::WrongProposer {
                height: block_height,
                allowed_rank,
            }),
            None => Ok(()),
        }
    }

    /// Collect proposer addresses across the previous epoch (the one
    /// ending right before `block_height`). Returns empty when there is
    /// no settlement to do (first epoch, no transition). Used to feed
    /// [`Self::apply_epoch_settlement_for_block`] now that the in-memory
    /// `Vec<Block>` is gone — the helper used to accept `&[Block]` and
    /// re-derive addresses, but the slice came from `self.blocks`. Going
    /// through `block_at_height` here lets the caller hand the helper a
    /// pre-computed `&[Vec<u8>]` without re-borrowing `self`.
    pub(super) fn proposer_addresses_for_settling_epoch(&self, block_height: u64) -> Vec<Vec<u8>> {
        let epoch_len = self.epoch_length.max(1);
        let prev_epoch = block_height.saturating_sub(1) / epoch_len;
        let new_epoch = block_height / epoch_len;
        if new_epoch <= prev_epoch || prev_epoch == 0 {
            return Vec::new();
        }
        let epoch_start = prev_epoch * epoch_len;
        let epoch_end = new_epoch * epoch_len;
        (epoch_start..epoch_end)
            .filter_map(|h| {
                self.block_at_height(h)
                    .map(|b| hash::address_bytes_from_public_key(&b.header.validator_public_key))
            })
            .collect()
    }

    /// Apply epoch settlement (rewards + inactivity penalties) when the block
    /// at `block_height` crosses an epoch boundary. The result mutates
    /// `accounts` in place and updates `validator_missed_epochs`.
    ///
    /// This is intentionally a free function over its inputs (no `&self`):
    /// `add_block` invokes it on `self.accounts` while the boot-time replay
    /// (`rebuild_canonical_state` / `replay_state_to_tip`) invokes it on a
    /// local accounts map. Keeping the body in one place is what guarantees
    /// the live state root and the recomputed-on-restart state root agree —
    /// the previous divergence caused the `state_root_mismatch` crash loop
    /// at every multiple of `epoch_length` past `2 * epoch_length`.
    ///
    /// The first epoch boundary (`prev_epoch == 0`) intentionally skips
    /// settlement for the genesis epoch, matching the historical guard in
    /// `add_block`.
    pub(super) fn apply_epoch_settlement_for_block(
        block_height: u64,
        epoch_length: u64,
        epoch_snapshots: &HashMap<u64, EpochSnapshot>,
        producer_addresses_in_prev_epoch: &[Vec<u8>],
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        validator_missed_epochs: &mut HashMap<Vec<u8>, u64>,
    ) {
        let epoch_len = epoch_length.max(1);
        let prev_epoch = block_height.saturating_sub(1) / epoch_len;
        let new_epoch = block_height / epoch_len;
        if new_epoch <= prev_epoch || prev_epoch == 0 {
            return;
        }
        let Some(snapshot) = epoch_snapshots.get(&prev_epoch) else {
            return;
        };

        // `producer_addresses_in_prev_epoch` is pre-computed by the caller
        // from `cursor.block_at(h)` over `epoch_start..epoch_end`. We just
        // tally — order doesn't matter, the histogram only cares about
        // counts per address.
        let mut block_producers: HashMap<Vec<u8>, u64> = HashMap::new();
        for addr in producer_addresses_in_prev_epoch {
            *block_producers.entry(addr.clone()).or_default() += 1;
        }

        let settlement = crate::consensus::compute_epoch_settlement(
            snapshot,
            &block_producers,
            validator_missed_epochs,
        );
        crate::consensus::apply_epoch_settlement(accounts, &settlement);

        // Update missed_epochs tracker: reset producers, increment non-producers.
        for validator in &snapshot.validators {
            if block_producers.contains_key(&validator.address) {
                validator_missed_epochs.remove(&validator.address);
            } else {
                *validator_missed_epochs
                    .entry(validator.address.clone())
                    .or_default() += 1;
            }
        }

        if settlement.total_rewards_distributed > 0 || settlement.total_penalties_applied > 0 {
            tracing::info!(
                target: "audit",
                event = "epoch_settlement",
                epoch = prev_epoch,
                rewards = settlement.total_rewards_distributed,
                penalties = settlement.total_penalties_applied,
            );
        }
    }

    /// Public helper: return the slot leader at the given (height, prev_hash, rank)
    /// using the chain's frozen epoch snapshots. Used by the network production
    /// loop to gate `create_block` calls — only the elected validator should
    /// build a block at every 10 s slot, with backups taking over after
    /// `BACKUP_LEADER_TIMEOUT_SECS` of silence.
    pub fn slot_leader_address(
        &self,
        block_height: u64,
        prev_hash: &[u8],
        rank: u32,
    ) -> Option<Vec<u8>> {
        let snapshot =
            self.snapshot_for_height(&self.accounts, &self.epoch_snapshots, block_height)?;
        if snapshot.validators.is_empty() {
            return None;
        }
        slot_leader_at_rank(&snapshot, block_height, prev_hash, rank)
    }
}
