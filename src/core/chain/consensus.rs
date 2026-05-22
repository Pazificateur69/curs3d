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
    AccountState, Blockchain, GenesisConfig, V6_HARDFORK_HEIGHT_TESTNET, V6_PROTOCOL_VERSION,
};
use crate::consensus::{EpochSnapshot, ProofOfStake, allowed_backup_rank};

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
}
