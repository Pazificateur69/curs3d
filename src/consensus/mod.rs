use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::block::{Block, BlockHeader};
use crate::core::chain::AccountState;
use crate::crypto::dilithium::{self, Signature};

/// Percentage of stake slashed on equivocation (33%)
pub const EQUIVOCATION_SLASH_PERCENT: u64 = 33;

/// Epoch reward rate: microtokens per CUR staked per epoch
pub const EPOCH_REWARD_RATE_PER_CUR: u64 = 100;

/// Inactivity penalty: microtokens per CUR staked per missed epoch
pub const INACTIVITY_PENALTY_RATE_PER_CUR: u64 = 50;

/// Number of consecutive missed epochs before penalty applies
pub const INACTIVITY_GRACE_EPOCHS: u64 = 2;

/// Target time between blocks (slot duration), in seconds.
pub const SLOT_DURATION_SECS: u64 = 10;

/// Time after the parent timestamp at which the rank-`k` backup may take
/// over from rank-`k-1`.
///
/// **3x the slot duration.** The previous value (12s, "1.2x the slot") was
/// too tight: with `block_timer` ticking every 10s on each node and gossip
/// latency of 1–8s on a small libp2p mesh, two validators frequently met
/// the backup threshold at the same wall-clock time before the primary's
/// block arrived. That manifested as parallel-fork production (the
/// 2026-05-05 incident: node2 and node3 both produced #270 at different
/// hashes 11 seconds apart, then drifted to h=287 before either side
/// could converge on the other).
///
/// 30s = `SLOT_DURATION_SECS + 2 * worst_case_gossip` gives the primary a
/// full slot to produce, plus enough headroom that even a slow gossip
/// path will deliver the primary's block before any backup wakes. An
/// offline primary still recovers within 30s (one missed slot), which is
/// well within human-operator tolerance for a testnet.
pub const BACKUP_LEADER_TIMEOUT_SECS: u64 = 30;

/// 20-byte CUR address. Aliased here so consensus signatures read as
/// "Address" rather than the more generic `Vec<u8>`.
pub type Address = Vec<u8>;

// ─── Validator ───────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Validator {
    pub address: Vec<u8>,
    pub public_key: Vec<u8>,
    pub stake: u64,
}

// ─── Equivocation Evidence ───────────────────────────────────────────

/// Proof that a validator signed two different blocks at the same height.
/// Only requires the two signed block hashes — not the full blocks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EquivocationEvidence {
    pub height: u64,
    pub validator_public_key: Vec<u8>,
    pub block_header_a: BlockHeader,
    pub block_hash_a: Vec<u8>,
    pub signature_a: Signature,
    pub block_header_b: BlockHeader,
    pub block_hash_b: Vec<u8>,
    pub signature_b: Signature,
}

impl EquivocationEvidence {
    /// Verify that this evidence is valid:
    /// - The two block hashes are different
    /// - Both hashes commit to headers at the claimed height
    /// - Both signatures are valid Dilithium5 signatures from the same validator
    pub fn verify(&self) -> bool {
        if self.block_hash_a == self.block_hash_b {
            return false;
        }

        if self.block_header_a.height != self.height || self.block_header_b.height != self.height {
            return false;
        }

        if self.block_header_a.validator_public_key != self.validator_public_key
            || self.block_header_b.validator_public_key != self.validator_public_key
        {
            return false;
        }

        if Block::compute_hash(&self.block_header_a) != self.block_hash_a
            || Block::compute_hash(&self.block_header_b) != self.block_hash_b
        {
            return false;
        }

        let sig_a_ok = dilithium::verify(
            &self.block_hash_a,
            &self.signature_a,
            &self.validator_public_key,
        );
        let sig_b_ok = dilithium::verify(
            &self.block_hash_b,
            &self.signature_b,
            &self.validator_public_key,
        );

        sig_a_ok && sig_b_ok
    }

    /// Create a unique key for storage deduplication
    pub fn key(&self) -> Vec<u8> {
        let mut k = self.height.to_be_bytes().to_vec();
        k.extend_from_slice(&self.validator_public_key);
        k
    }
}

#[derive(Debug, Error)]
pub enum SlashingError {
    #[error("invalid equivocation evidence")]
    InvalidEvidence,
    #[error("validator already slashed for this offense")]
    AlreadySlashed,
    #[error("validator not found")]
    ValidatorNotFound,
}

// ─── Finality Vote ───────────────────────────────────────────────────

/// A validator's attestation that a block should be finalized.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalityVote {
    pub block_hash: Vec<u8>,
    pub block_height: u64,
    pub epoch: u64,
    pub voter_public_key: Vec<u8>,
    pub signature: Signature,
}

impl FinalityVote {
    /// Domain-separated signable data for finality votes
    fn signable_data(block_hash: &[u8], block_height: u64, epoch: u64) -> Vec<u8> {
        let mut data = b"curs3d-finality-vote-v1:".to_vec();
        data.extend_from_slice(block_hash);
        data.extend_from_slice(&block_height.to_le_bytes());
        data.extend_from_slice(&epoch.to_le_bytes());
        data
    }

    pub fn new(
        block_hash: Vec<u8>,
        block_height: u64,
        epoch: u64,
        keypair: &crate::crypto::dilithium::KeyPair,
    ) -> Self {
        let data = Self::signable_data(&block_hash, block_height, epoch);
        let signature = keypair.sign(&data);
        FinalityVote {
            block_hash,
            block_height,
            epoch,
            voter_public_key: keypair.public_key.clone(),
            signature,
        }
    }

    pub fn verify(&self) -> bool {
        let data = Self::signable_data(&self.block_hash, self.block_height, self.epoch);
        dilithium::verify(&data, &self.signature, &self.voter_public_key)
    }
}

/// Returned when a block reaches finality
#[derive(Clone, Debug)]
pub struct FinalizedBlock {
    pub hash: Vec<u8>,
    pub height: u64,
}

/// Tracks finality votes and determines when 2/3+ threshold is reached.
pub struct FinalityTracker {
    /// block_hash -> list of votes for that block
    votes: HashMap<Vec<u8>, Vec<FinalityVote>>,
    /// Per-validator dedup set keyed by (validator_public_key, height).
    /// A validator may only count once per height across the chain — a second
    /// vote at the same height (even for a different block hash) is rejected
    /// here. Equivocation evidence handles the slashing path elsewhere.
    seen_votes: HashSet<(Vec<u8>, u64)>,
    pub finalized_height: u64,
    pub finalized_hash: Vec<u8>,
}

impl Default for FinalityTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FinalityTracker {
    pub fn new() -> Self {
        FinalityTracker {
            votes: HashMap::new(),
            seen_votes: HashSet::new(),
            finalized_height: 0,
            finalized_hash: Vec::new(),
        }
    }

    /// Load from persisted state
    pub fn with_finalized(height: u64, hash: Vec<u8>) -> Self {
        FinalityTracker {
            votes: HashMap::new(),
            seen_votes: HashSet::new(),
            finalized_height: height,
            finalized_hash: hash,
        }
    }

    /// Add a vote. Returns Some(FinalizedBlock) if this vote pushes the block
    /// past the 2/3 threshold.
    pub fn add_vote(
        &mut self,
        vote: FinalityVote,
        snapshot: &EpochSnapshot,
    ) -> Option<FinalizedBlock> {
        if !vote.verify() {
            return None;
        }

        if vote.epoch != snapshot.epoch {
            return None;
        }

        // Only votes for blocks above current finalized height matter
        if vote.block_height <= self.finalized_height {
            return None;
        }

        let validator_stakes: HashMap<Vec<u8>, u64> = snapshot
            .validators
            .iter()
            .map(|validator| (validator.public_key.clone(), validator.stake))
            .collect();
        if !validator_stakes.contains_key(&vote.voter_public_key) {
            return None;
        }

        // Replay protection: reject any second vote from the same validator at
        // the same height, regardless of block_hash. (Equivocation = slashable
        // offence, but we still must not let it count twice toward finality.)
        let dedup_key = (vote.voter_public_key.clone(), vote.block_height);
        if !self.seen_votes.insert(dedup_key) {
            return None;
        }

        let block_hash = vote.block_hash.clone();
        let block_height = vote.block_height;

        {
            let votes = self.votes.entry(block_hash.clone()).or_default();
            if votes
                .iter()
                .any(|v| v.voter_public_key == vote.voter_public_key)
            {
                return None; // Already voted (defence in depth — should be unreachable)
            }
            votes.push(vote);
        }

        // Check if 2/3+ threshold reached
        let total_active_stake = snapshot.total_stake;
        if total_active_stake == 0 {
            return None;
        }

        let voted_stake: u64 = self
            .votes
            .get(&block_hash)
            .map(|votes| {
                votes
                    .iter()
                    .filter_map(|v| validator_stakes.get(&v.voter_public_key).copied())
                    .sum()
            })
            .unwrap_or(0);

        // 2/3 threshold: voted_stake * 3 >= total_active_stake * 2
        if voted_stake.saturating_mul(3) >= total_active_stake.saturating_mul(2) {
            self.finalized_height = block_height;
            self.finalized_hash = block_hash.clone();

            // Prune old votes for blocks at or below finalized height
            self.votes.retain(|_, v| {
                v.first()
                    .map(|f| f.block_height > block_height)
                    .unwrap_or(false)
            });
            // Prune dedup entries for finalized heights — they can never be
            // replayed against finality again, so keeping them only wastes RAM.
            self.seen_votes.retain(|(_, h)| *h > block_height);

            Some(FinalizedBlock {
                hash: block_hash,
                height: block_height,
            })
        } else {
            None
        }
    }
}

// ─── Epoch Snapshot ─────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpochSnapshot {
    pub epoch: u64,
    pub start_height: u64,
    pub validators: Vec<Validator>,
    pub total_stake: u64,
}

// ─── Epoch Settlement (Rewards + Inactivity Penalties) ──────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EpochSettlement {
    pub epoch: u64,
    pub rewards: Vec<ValidatorReward>,
    pub penalties: Vec<ValidatorPenalty>,
    pub total_rewards_distributed: u64,
    pub total_penalties_applied: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatorReward {
    pub address: Vec<u8>,
    pub amount: u64,
    pub blocks_produced: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidatorPenalty {
    pub address: Vec<u8>,
    pub amount: u64,
    pub reason: PenaltyReason,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PenaltyReason {
    Inactivity { missed_epochs: u64 },
    Equivocation,
}

/// Compute epoch settlement: rewards for active validators, penalties for inactive ones.
///
/// `block_producers` maps validator addresses to the number of blocks they produced in this epoch.
/// `missed_epochs` maps validator addresses to how many consecutive epochs they've missed.
pub fn compute_epoch_settlement(
    snapshot: &EpochSnapshot,
    block_producers: &HashMap<Vec<u8>, u64>,
    missed_epochs: &HashMap<Vec<u8>, u64>,
) -> EpochSettlement {
    let mut rewards = Vec::new();
    let mut penalties = Vec::new();
    let mut total_rewards = 0u64;
    let mut total_penalties = 0u64;

    for validator in &snapshot.validators {
        let blocks_produced = block_producers
            .get(&validator.address)
            .copied()
            .unwrap_or(0);

        if blocks_produced > 0 {
            // Reward: proportional to stake and blocks produced
            let stake_in_cur = validator.stake / 1_000_000; // microtokens to CUR
            let reward = stake_in_cur
                .saturating_mul(EPOCH_REWARD_RATE_PER_CUR)
                .saturating_mul(blocks_produced);
            if reward > 0 {
                rewards.push(ValidatorReward {
                    address: validator.address.clone(),
                    amount: reward,
                    blocks_produced,
                });
                total_rewards = total_rewards.saturating_add(reward);
            }
        } else {
            // Check inactivity
            let consecutive_missed = missed_epochs
                .get(&validator.address)
                .copied()
                .unwrap_or(0)
                .saturating_add(1); // This epoch counts as missed too

            if consecutive_missed > INACTIVITY_GRACE_EPOCHS {
                let stake_in_cur = validator.stake / 1_000_000;
                let penalty = stake_in_cur
                    .saturating_mul(INACTIVITY_PENALTY_RATE_PER_CUR)
                    .saturating_mul(consecutive_missed.saturating_sub(INACTIVITY_GRACE_EPOCHS));
                if penalty > 0 {
                    penalties.push(ValidatorPenalty {
                        address: validator.address.clone(),
                        amount: penalty,
                        reason: PenaltyReason::Inactivity {
                            missed_epochs: consecutive_missed,
                        },
                    });
                    total_penalties = total_penalties.saturating_add(penalty);
                }
            }
        }
    }

    EpochSettlement {
        epoch: snapshot.epoch,
        rewards,
        penalties,
        total_rewards_distributed: total_rewards,
        total_penalties_applied: total_penalties,
    }
}

/// Apply an epoch settlement to account balances.
pub fn apply_epoch_settlement(
    accounts: &mut HashMap<Vec<u8>, AccountState>,
    settlement: &EpochSettlement,
) {
    // Apply rewards (add to liquid balance)
    for reward in &settlement.rewards {
        let account = accounts.entry(reward.address.clone()).or_default();
        account.balance = account.balance.saturating_add(reward.amount);
    }

    // Apply penalties (deduct from staked balance)
    for penalty in &settlement.penalties {
        let account = accounts.entry(penalty.address.clone()).or_default();
        account.staked_balance = account.staked_balance.saturating_sub(penalty.amount);
    }
}

// ─── Proof of Stake ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct ProofOfStake {
    pub minimum_stake: u64,
    pub slashed_validators: HashSet<Vec<u8>>,
    pub current_height: u64,
}

impl ProofOfStake {
    #[allow(dead_code)]
    pub fn new(minimum_stake: u64, current_height: u64) -> Self {
        ProofOfStake {
            minimum_stake,
            slashed_validators: HashSet::new(),
            current_height,
        }
    }

    pub fn with_slashed(
        minimum_stake: u64,
        slashed: HashSet<Vec<u8>>,
        current_height: u64,
    ) -> Self {
        ProofOfStake {
            minimum_stake,
            slashed_validators: slashed,
            current_height,
        }
    }

    pub fn active_validators(&self, accounts: &HashMap<Vec<u8>, AccountState>) -> Vec<Validator> {
        let mut validators: Vec<Validator> = accounts
            .iter()
            .filter_map(|(address, account)| {
                let public_key = account.public_key.clone()?;
                if account.staked_balance < self.minimum_stake {
                    return None;
                }
                if account.validator_active_from_height > self.current_height {
                    return None;
                }
                if account.jailed_until_height > self.current_height {
                    return None;
                }
                // Permanently slashed validators cannot participate
                if self.slashed_validators.contains(address) {
                    return None;
                }
                Some(Validator {
                    address: address.clone(),
                    public_key,
                    stake: account.staked_balance,
                })
            })
            .collect();

        validators.sort_by(|a, b| a.address.cmp(&b.address));
        validators
    }

    pub fn select_validator(
        &self,
        accounts: &HashMap<Vec<u8>, AccountState>,
        block_height: u64,
        prev_hash: &[u8],
    ) -> Option<Validator> {
        let validators = self.active_validators(accounts);
        if validators.is_empty() {
            return None;
        }

        let total_stake: u64 = validators.iter().map(|v| v.stake).sum();
        if total_stake == 0 {
            return None;
        }

        let hash = crate::crypto::hash::sha3_hash_domain(
            b"curs3d-validator-selection",
            &[&block_height.to_le_bytes(), prev_hash],
        );
        // SHA-3-256 always returns 32 bytes; taking first 8 is safe
        let mut selector = u64::from_le_bytes([
            hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
        ]) % total_stake;

        for validator in validators {
            if selector < validator.stake {
                return Some(validator);
            }
            selector -= validator.stake;
        }

        None
    }

    /// Select a validator from a frozen epoch snapshot using the same weighted selection logic.
    pub fn select_validator_from_snapshot(
        snapshot: &EpochSnapshot,
        block_height: u64,
        prev_hash: &[u8],
    ) -> Option<Validator> {
        if snapshot.validators.is_empty() {
            return None;
        }

        let total_stake: u64 = snapshot.validators.iter().map(|v| v.stake).sum();
        if total_stake == 0 {
            return None;
        }

        let hash = crate::crypto::hash::sha3_hash_domain(
            b"curs3d-validator-selection",
            &[&block_height.to_le_bytes(), prev_hash],
        );
        let mut selector = u64::from_le_bytes([
            hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
        ]) % total_stake;

        for validator in &snapshot.validators {
            if selector < validator.stake {
                return Some(validator.clone());
            }
            selector -= validator.stake;
        }

        None
    }

    /// Slash a validator with cryptographic proof of equivocation.
    /// Returns the penalty amount on success.
    pub fn slash_with_evidence(
        &mut self,
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        evidence: &EquivocationEvidence,
        jail_duration_blocks: u64,
    ) -> Result<u64, SlashingError> {
        if !evidence.verify() {
            return Err(SlashingError::InvalidEvidence);
        }

        let address =
            crate::crypto::hash::address_bytes_from_public_key(&evidence.validator_public_key);

        if self.slashed_validators.contains(&address) {
            return Err(SlashingError::AlreadySlashed);
        }

        let account = accounts
            .get_mut(&address)
            .ok_or(SlashingError::ValidatorNotFound)?;

        let penalty = account
            .staked_balance
            .saturating_mul(EQUIVOCATION_SLASH_PERCENT)
            / 100;
        account.staked_balance = account.staked_balance.saturating_sub(penalty);
        account.jailed_until_height = self.current_height.saturating_add(jail_duration_blocks);

        self.slashed_validators.insert(address);

        Ok(penalty)
    }

    /// Legacy slash method (percentage-based, no proof required)
    #[deprecated(note = "Use slash_with_evidence for provable slashing")]
    #[allow(dead_code)]
    pub fn slash(
        &self,
        accounts: &mut HashMap<Vec<u8>, AccountState>,
        address: &[u8],
        penalty_percent: u64,
    ) -> Option<u64> {
        let account = accounts.get_mut(address)?;
        let penalty = account.staked_balance.saturating_mul(penalty_percent) / 100;
        account.staked_balance = account.staked_balance.saturating_sub(penalty);
        Some(penalty)
    }
}

// ─── Slot-leader scheduling ─────────────────────────────────────────
//
// Deterministic stake-weighted election of the validator entitled to
// propose at a given height. The schedule is a pure function of the
// epoch snapshot, the height, and the parent block hash, so all honest
// validators with the same chain state agree on who is supposed to
// build the next block — eliminating the parallel-fork problem we hit
// with ≥2 validators all proposing at every 10 s tick.
//
// The output is also lightly unpredictable to outside observers: an
// attacker that can guess the validator set still cannot pre-compute
// the schedule without knowing the parent block hash, which only
// settles when the previous block lands.

/// Deterministically selects the slot leader for a given height.
///
/// `epoch_snapshot` is the validator set frozen at the epoch boundary.
/// The seed mixes the parent block hash so the schedule is unpredictable
/// to outside attackers but deterministic to all validators with the same
/// chain state.
///
/// Selection is stake-weighted: each validator gets a "ticket-range"
/// proportional to their staked balance. The selected ticket is
/// `sha3(height || parent_hash || "curs3d-slot-leader-v1") mod total_stake`.
///
/// Returns `None` if the snapshot has no validators or zero total stake;
/// callers usually treat that as "any validator may propose" (genesis
/// edge case, single-node bootstrap).
pub fn slot_leader(
    epoch_snapshot: &EpochSnapshot,
    height: u64,
    parent_hash: &[u8],
) -> Option<Address> {
    slot_leader_at_rank(epoch_snapshot, height, parent_hash, 0)
}

/// Returns the k-th backup leader for a slot (k=0 is the primary).
///
/// After ~`BACKUP_LEADER_TIMEOUT_SECS` of no block at the expected
/// height, the rank-1 backup may take over. After another timeout,
/// rank-2, etc. We keep selecting deterministically by re-seeding the
/// hash with `rank` so every node converges on the same backup.
///
/// The backup at rank `k` is chosen from the snapshot validators
/// **excluding** the primary and all earlier backups, so a 3-validator
/// ring iterates through all three before wrapping. With only 1
/// validator left, that validator is returned for every higher rank.
pub fn slot_leader_at_rank(
    epoch_snapshot: &EpochSnapshot,
    height: u64,
    parent_hash: &[u8],
    rank: u32,
) -> Option<Address> {
    if epoch_snapshot.validators.is_empty() {
        return None;
    }

    // Walk through ranks 0..=rank, each time excluding the previously
    // chosen leaders so backups never duplicate the primary.
    let mut excluded: HashSet<Address> = HashSet::new();
    let mut last: Option<Address> = None;
    for k in 0..=rank {
        let chosen = slot_leader_excluding(epoch_snapshot, height, parent_hash, k, &excluded);
        match chosen {
            Some(addr) => {
                excluded.insert(addr.clone());
                last = Some(addr);
            }
            None => {
                // Pool exhausted — return the previously chosen leader
                // (the highest rank achievable). Guarantees liveness
                // even when `rank > N - 1`.
                return last;
            }
        }
    }
    last
}

/// Internal: pick the stake-weighted leader at a given rank, skipping
/// any address in `excluded`. Returns `None` if all candidates are
/// excluded or total remaining stake is zero.
fn slot_leader_excluding(
    epoch_snapshot: &EpochSnapshot,
    height: u64,
    parent_hash: &[u8],
    rank: u32,
    excluded: &HashSet<Address>,
) -> Option<Address> {
    let candidates: Vec<&Validator> = epoch_snapshot
        .validators
        .iter()
        .filter(|v| !excluded.contains(&v.address))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let total_stake: u64 = candidates.iter().map(|v| v.stake).sum();
    if total_stake == 0 {
        return None;
    }

    let hash = crate::crypto::hash::sha3_hash_domain(
        b"curs3d-slot-leader-v1",
        &[&height.to_le_bytes(), parent_hash, &rank.to_le_bytes()],
    );
    // SHA3-256 returns 32 bytes. Use the first 16 bytes as a u128 to
    // sample a wide range before the modulo, avoiding modulo bias
    // on small total_stake values.
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&hash[..16]);
    let selector = u128::from_le_bytes(buf) % (total_stake as u128);
    let mut selector = selector as u64;

    for v in &candidates {
        if selector < v.stake {
            return Some(v.address.clone());
        }
        selector -= v.stake;
    }
    // Numerically unreachable (we already checked total_stake > 0 and
    // selector < total_stake), but keep a safe fallback rather than
    // panic in a consensus path.
    candidates.last().map(|v| v.address.clone())
}

/// Compute the maximum backup rank that may have taken over given the
/// elapsed time since the parent block.
///
/// At t = parent_timestamp + BACKUP_LEADER_TIMEOUT_SECS the rank-1
/// backup may produce; at +2*timeout, rank-2; etc. The result is
/// clamped to the snapshot's validator count - 1 so we never permit
/// an out-of-range rank.
pub fn allowed_backup_rank(
    parent_timestamp: i64,
    block_timestamp: i64,
    snapshot_validators: usize,
) -> u32 {
    if snapshot_validators <= 1 || block_timestamp <= parent_timestamp {
        return 0;
    }
    let gap = (block_timestamp - parent_timestamp) as u64;
    if gap < BACKUP_LEADER_TIMEOUT_SECS {
        return 0;
    }
    let rank = gap / BACKUP_LEADER_TIMEOUT_SECS;
    let max_rank = snapshot_validators.saturating_sub(1) as u64;
    rank.min(max_rank) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::dilithium::KeyPair;
    use crate::crypto::hash;

    fn signed_header(
        kp: &KeyPair,
        height: u64,
        timestamp: i64,
        prev_hash: Vec<u8>,
        merkle_seed: &[u8],
    ) -> (BlockHeader, Vec<u8>, Signature) {
        let header = BlockHeader {
            version: 1,
            height,
            timestamp,
            prev_hash,
            merkle_root: hash::sha3_hash(merkle_seed),
            state_root: hash::sha3_hash(b"state"),
            gas_used: 0,
            base_fee_per_gas: 1,
            validator_public_key: kp.public_key.clone(),
            nonce: 0,
        };
        let block_hash = Block::compute_hash(&header);
        let signature = kp.sign(&block_hash);
        (header, block_hash, signature)
    }

    #[test]
    fn test_active_validators() {
        let kp = KeyPair::generate();
        let address = hash::address_bytes_from_public_key(&kp.public_key);
        let mut accounts = HashMap::new();
        accounts.insert(
            address.clone(),
            AccountState {
                balance: 10,
                nonce: 0,
                staked_balance: 5_000,
                pending_unstakes: Vec::new(),
                validator_active_from_height: 0,
                jailed_until_height: 0,
                public_key: Some(kp.public_key.clone()),
            },
        );

        let pos = ProofOfStake::new(1_000, 1);
        let validators = pos.active_validators(&accounts);
        assert_eq!(validators.len(), 1);
        assert_eq!(validators[0].address, address);
    }

    #[test]
    fn test_select_validator() {
        let kp1 = KeyPair::generate();
        let kp2 = KeyPair::generate();
        let mut accounts = HashMap::new();
        accounts.insert(
            hash::address_bytes_from_public_key(&kp1.public_key),
            AccountState {
                balance: 0,
                nonce: 0,
                staked_balance: 5_000,
                pending_unstakes: Vec::new(),
                validator_active_from_height: 0,
                jailed_until_height: 0,
                public_key: Some(kp1.public_key.clone()),
            },
        );
        accounts.insert(
            hash::address_bytes_from_public_key(&kp2.public_key),
            AccountState {
                balance: 0,
                nonce: 0,
                staked_balance: 3_000,
                pending_unstakes: Vec::new(),
                validator_active_from_height: 0,
                jailed_until_height: 0,
                public_key: Some(kp2.public_key.clone()),
            },
        );

        let pos = ProofOfStake::new(1_000, 1);
        let selected = pos.select_validator(&accounts, 1, &[0; 32]);
        assert!(selected.is_some());
    }

    #[test]
    #[allow(deprecated)]
    fn test_slashing() {
        let kp = KeyPair::generate();
        let address = hash::address_bytes_from_public_key(&kp.public_key);
        let mut accounts = HashMap::new();
        accounts.insert(
            address.clone(),
            AccountState {
                balance: 0,
                nonce: 0,
                staked_balance: 5_000,
                pending_unstakes: Vec::new(),
                validator_active_from_height: 0,
                jailed_until_height: 0,
                public_key: Some(kp.public_key.clone()),
            },
        );

        let pos = ProofOfStake::new(1_000, 1);
        let penalty = pos.slash(&mut accounts, &address, 50);
        assert_eq!(penalty, Some(2_500));
        assert_eq!(accounts[&address].staked_balance, 2_500);
    }

    #[test]
    fn test_equivocation_evidence_valid() {
        let kp = KeyPair::generate();
        let (header_a, hash_a, sig_a) = signed_header(&kp, 5, 100, vec![0; 32], b"block_a");
        let (header_b, hash_b, sig_b) = signed_header(&kp, 5, 101, vec![1; 32], b"block_b");

        let evidence = EquivocationEvidence {
            height: 5,
            validator_public_key: kp.public_key.clone(),
            block_header_a: header_a,
            block_hash_a: hash_a,
            signature_a: sig_a,
            block_header_b: header_b,
            block_hash_b: hash_b,
            signature_b: sig_b,
        };

        assert!(evidence.verify());
    }

    #[test]
    fn test_equivocation_evidence_same_hash_rejected() {
        let kp = KeyPair::generate();
        let (header_a, hash_a, sig_a) = signed_header(&kp, 5, 100, vec![0; 32], b"same_block");
        let (header_b, _hash_b, sig_b) = signed_header(&kp, 5, 101, vec![1; 32], b"same_block_2");

        let evidence = EquivocationEvidence {
            height: 5,
            validator_public_key: kp.public_key.clone(),
            block_header_a: header_a,
            block_hash_a: hash_a.clone(),
            signature_a: sig_a,
            block_header_b: header_b,
            block_hash_b: hash_a,
            signature_b: sig_b,
        };

        assert!(!evidence.verify());
    }

    #[test]
    fn test_slash_with_valid_evidence() {
        let kp = KeyPair::generate();
        let address = hash::address_bytes_from_public_key(&kp.public_key);
        let mut accounts = HashMap::new();
        accounts.insert(
            address.clone(),
            AccountState {
                balance: 100,
                nonce: 0,
                staked_balance: 9_000,
                pending_unstakes: Vec::new(),
                validator_active_from_height: 0,
                jailed_until_height: 0,
                public_key: Some(kp.public_key.clone()),
            },
        );

        let (header_a, hash_a, sig_a) = signed_header(&kp, 5, 100, vec![0; 32], b"block_a");
        let (header_b, hash_b, sig_b) = signed_header(&kp, 5, 101, vec![1; 32], b"block_b");
        let evidence = EquivocationEvidence {
            height: 5,
            validator_public_key: kp.public_key.clone(),
            block_header_a: header_a,
            block_hash_a: hash_a,
            signature_a: sig_a,
            block_header_b: header_b,
            block_hash_b: hash_b,
            signature_b: sig_b,
        };

        let mut pos = ProofOfStake::new(1_000, 5);
        let penalty = pos
            .slash_with_evidence(&mut accounts, &evidence, 10)
            .unwrap();
        assert_eq!(penalty, 2_970); // 33% of 9000
        assert_eq!(accounts[&address].staked_balance, 6_030);
        assert_eq!(accounts[&address].jailed_until_height, 15);
        assert!(pos.slashed_validators.contains(&address));
    }

    #[test]
    fn test_equivocation_evidence_rejects_mismatched_height() {
        let kp = KeyPair::generate();
        let (header_a, hash_a, sig_a) = signed_header(&kp, 5, 100, vec![0; 32], b"block_a");
        let (header_b, hash_b, sig_b) = signed_header(&kp, 6, 101, vec![1; 32], b"block_b");
        let evidence = EquivocationEvidence {
            height: 5,
            validator_public_key: kp.public_key.clone(),
            block_header_a: header_a,
            block_hash_a: hash_a,
            signature_a: sig_a,
            block_header_b: header_b,
            block_hash_b: hash_b,
            signature_b: sig_b,
        };

        assert!(!evidence.verify());
    }

    #[test]
    fn test_jailed_validator_excluded() {
        let kp = KeyPair::generate();
        let address = hash::address_bytes_from_public_key(&kp.public_key);
        let mut accounts = HashMap::new();
        accounts.insert(
            address.clone(),
            AccountState {
                balance: 0,
                nonce: 0,
                staked_balance: 5_000,
                pending_unstakes: Vec::new(),
                validator_active_from_height: 0,
                jailed_until_height: 10,
                public_key: Some(kp.public_key.clone()),
            },
        );

        let mut slashed = HashSet::new();
        slashed.insert(address.clone());
        let pos = ProofOfStake::with_slashed(1_000, slashed, 1);

        assert_eq!(pos.active_validators(&accounts).len(), 0);
        assert!(pos.select_validator(&accounts, 1, &[0; 32]).is_none());
    }

    #[test]
    fn test_finality_vote_verify() {
        let kp = KeyPair::generate();
        let block_hash = hash::sha3_hash(b"test_block");
        let vote = FinalityVote::new(block_hash, 10, 0, &kp);
        assert!(vote.verify());
    }

    #[test]
    fn test_finality_reached_at_two_thirds() {
        let kp1 = KeyPair::generate();
        let kp2 = KeyPair::generate();
        let kp3 = KeyPair::generate();

        let addr1 = hash::address_bytes_from_public_key(&kp1.public_key);
        let addr2 = hash::address_bytes_from_public_key(&kp2.public_key);
        let addr3 = hash::address_bytes_from_public_key(&kp3.public_key);

        let mut accounts = HashMap::new();
        for (addr, pk) in [
            (&addr1, &kp1.public_key),
            (&addr2, &kp2.public_key),
            (&addr3, &kp3.public_key),
        ] {
            accounts.insert(
                addr.clone(),
                AccountState {
                    balance: 0,
                    nonce: 0,
                    staked_balance: 3_000,
                    pending_unstakes: Vec::new(),
                    validator_active_from_height: 0,
                    jailed_until_height: 0,
                    public_key: Some(pk.clone()),
                },
            );
        }

        let mut tracker = FinalityTracker::new();
        let block_hash = hash::sha3_hash(b"block_to_finalize");
        let snapshot = EpochSnapshot {
            epoch: 0,
            start_height: 0,
            validators: vec![
                Validator {
                    address: addr1.clone(),
                    public_key: kp1.public_key.clone(),
                    stake: 3_000,
                },
                Validator {
                    address: addr2.clone(),
                    public_key: kp2.public_key.clone(),
                    stake: 3_000,
                },
                Validator {
                    address: addr3.clone(),
                    public_key: kp3.public_key.clone(),
                    stake: 3_000,
                },
            ],
            total_stake: 9_000,
        };

        // 1/3 vote — not enough
        let vote1 = FinalityVote::new(block_hash.clone(), 5, 0, &kp1);
        assert!(tracker.add_vote(vote1, &snapshot).is_none());

        // 2/3 votes — threshold reached
        let vote2 = FinalityVote::new(block_hash.clone(), 5, 0, &kp2);
        let result = tracker.add_vote(vote2, &snapshot);
        assert!(result.is_some());
        assert_eq!(result.unwrap().height, 5);
        assert_eq!(tracker.finalized_height, 5);
    }

    #[test]
    fn test_vote_deduplication() {
        let kp = KeyPair::generate();
        let addr = hash::address_bytes_from_public_key(&kp.public_key);
        let mut accounts = HashMap::new();
        accounts.insert(
            addr,
            AccountState {
                balance: 0,
                nonce: 0,
                staked_balance: 10_000,
                pending_unstakes: Vec::new(),
                validator_active_from_height: 0,
                jailed_until_height: 0,
                public_key: Some(kp.public_key.clone()),
            },
        );

        let mut tracker = FinalityTracker::new();
        let block_hash = hash::sha3_hash(b"block");
        let snapshot = EpochSnapshot {
            epoch: 0,
            start_height: 0,
            validators: vec![Validator {
                address: hash::address_bytes_from_public_key(&kp.public_key),
                public_key: kp.public_key.clone(),
                stake: 10_000,
            }],
            total_stake: 10_000,
        };

        let vote1 = FinalityVote::new(block_hash.clone(), 5, 0, &kp);
        let vote2 = FinalityVote::new(block_hash.clone(), 5, 0, &kp);

        // First vote triggers finality (single validator = 100% > 66%)
        assert!(tracker.add_vote(vote1, &snapshot).is_some());
        // Duplicate is ignored (block already finalized, height <= finalized)
        assert!(tracker.add_vote(vote2, &snapshot).is_none());
    }

    #[test]
    fn test_finality_rejects_replay_across_block_hashes() {
        // A validator must not be able to count twice toward finality at the
        // same height, even by changing the block hash they vote for.
        let kp = KeyPair::generate();
        let snapshot = EpochSnapshot {
            epoch: 0,
            start_height: 0,
            validators: vec![
                Validator {
                    address: hash::address_bytes_from_public_key(&kp.public_key),
                    public_key: kp.public_key.clone(),
                    stake: 5_000,
                },
                Validator {
                    address: vec![2u8; 20],
                    public_key: vec![2u8; 32],
                    stake: 5_000,
                },
            ],
            total_stake: 10_000,
        };
        let mut tracker = FinalityTracker::new();
        let hash_a = hash::sha3_hash(b"block-a");
        let hash_b = hash::sha3_hash(b"block-b");

        let vote_a = FinalityVote::new(hash_a, 5, 0, &kp);
        let vote_b = FinalityVote::new(hash_b, 5, 0, &kp);

        // First vote is accepted but doesn't reach 2/3 (we only have 50%).
        assert!(tracker.add_vote(vote_a, &snapshot).is_none());
        // Second vote at the same height is rejected by the cross-hash dedup
        // even though the block hash differs.
        assert!(tracker.add_vote(vote_b, &snapshot).is_none());
    }

    #[test]
    fn test_epoch_settlement_rewards() {
        let snapshot = EpochSnapshot {
            epoch: 1,
            start_height: 32,
            validators: vec![Validator {
                address: vec![1u8; 20],
                public_key: vec![1u8; 32],
                stake: 10_000_000_000, // 10,000 CUR
            }],
            total_stake: 10_000_000_000,
        };
        let mut producers = HashMap::new();
        producers.insert(vec![1u8; 20], 5u64); // produced 5 blocks
        let missed = HashMap::new();

        let settlement = compute_epoch_settlement(&snapshot, &producers, &missed);
        assert_eq!(settlement.rewards.len(), 1);
        assert_eq!(settlement.penalties.len(), 0);
        // 10,000 CUR * 100 rate * 5 blocks = 5,000,000 microtokens
        assert_eq!(settlement.rewards[0].amount, 5_000_000);
        assert_eq!(settlement.rewards[0].blocks_produced, 5);
    }

    #[test]
    fn test_epoch_settlement_inactivity_penalty() {
        let snapshot = EpochSnapshot {
            epoch: 5,
            start_height: 160,
            validators: vec![Validator {
                address: vec![2u8; 20],
                public_key: vec![2u8; 32],
                stake: 5_000_000_000, // 5,000 CUR
            }],
            total_stake: 5_000_000_000,
        };
        let producers = HashMap::new(); // produced nothing
        let mut missed = HashMap::new();
        missed.insert(vec![2u8; 20], 3u64); // already missed 3 epochs

        let settlement = compute_epoch_settlement(&snapshot, &producers, &missed);
        assert_eq!(settlement.rewards.len(), 0);
        assert_eq!(settlement.penalties.len(), 1);
        // missed 3+1=4, grace=2, penalty epochs=2
        // 5,000 CUR * 50 rate * 2 = 500,000 microtokens
        assert_eq!(settlement.penalties[0].amount, 500_000);
    }

    #[test]
    fn test_epoch_settlement_grace_period() {
        let snapshot = EpochSnapshot {
            epoch: 2,
            start_height: 64,
            validators: vec![Validator {
                address: vec![3u8; 20],
                public_key: vec![3u8; 32],
                stake: 1_000_000_000,
            }],
            total_stake: 1_000_000_000,
        };
        let producers = HashMap::new();
        let mut missed = HashMap::new();
        missed.insert(vec![3u8; 20], 1u64); // missed 1 epoch (+ this one = 2, still within grace)

        let settlement = compute_epoch_settlement(&snapshot, &producers, &missed);
        // 2 missed <= INACTIVITY_GRACE_EPOCHS (2), no penalty
        assert_eq!(settlement.penalties.len(), 0);
    }

    #[test]
    fn test_apply_epoch_settlement() {
        let mut accounts = HashMap::new();
        accounts.insert(
            vec![1u8; 20],
            AccountState {
                balance: 1_000_000,
                staked_balance: 10_000_000_000,
                ..Default::default()
            },
        );
        accounts.insert(
            vec![2u8; 20],
            AccountState {
                balance: 500_000,
                staked_balance: 5_000_000_000,
                ..Default::default()
            },
        );

        let settlement = EpochSettlement {
            epoch: 1,
            rewards: vec![ValidatorReward {
                address: vec![1u8; 20],
                amount: 1_000_000,
                blocks_produced: 10,
            }],
            penalties: vec![ValidatorPenalty {
                address: vec![2u8; 20],
                amount: 250_000,
                reason: PenaltyReason::Inactivity { missed_epochs: 3 },
            }],
            total_rewards_distributed: 1_000_000,
            total_penalties_applied: 250_000,
        };

        apply_epoch_settlement(&mut accounts, &settlement);

        assert_eq!(accounts[&vec![1u8; 20]].balance, 2_000_000); // 1M + 1M reward
        assert_eq!(accounts[&vec![2u8; 20]].staked_balance, 4_999_750_000); // 5B - 250K penalty
    }

    // ─── Slot-leader scheduling tests ───────────────────────────────

    fn snapshot_with_stakes(stakes: &[u64]) -> EpochSnapshot {
        let validators: Vec<Validator> = stakes
            .iter()
            .enumerate()
            .map(|(i, stake)| Validator {
                address: vec![(i + 1) as u8; 20],
                public_key: vec![(i + 1) as u8; 32],
                stake: *stake,
            })
            .collect();
        let total_stake: u64 = validators.iter().map(|v| v.stake).sum();
        EpochSnapshot {
            epoch: 0,
            start_height: 0,
            validators,
            total_stake,
        }
    }

    #[test]
    fn test_slot_leader_deterministic() {
        let snap = snapshot_with_stakes(&[1_000, 2_000, 3_000]);
        let parent = hash::sha3_hash(b"parent");
        for height in 1..50 {
            let a = slot_leader(&snap, height, &parent);
            let b = slot_leader(&snap, height, &parent);
            assert_eq!(
                a, b,
                "slot_leader must be deterministic at height {}",
                height
            );
        }
    }

    #[test]
    fn test_slot_leader_stake_weighted() {
        // Stakes 1:2:3 → expect roughly 1/6, 2/6, 3/6 share over 1000 slots.
        let snap = snapshot_with_stakes(&[1_000, 2_000, 3_000]);
        let parent = hash::sha3_hash(b"weighted-parent");
        let mut counts = [0u64; 3];
        let n = 1000u64;
        for height in 1..=n {
            let leader = slot_leader(&snap, height, &parent).unwrap();
            counts[(leader[0] - 1) as usize] += 1;
        }
        // Expected: ~167, ~333, ~500. Allow ±10% of n (i.e. ±100 slots).
        let tolerance = (n / 10) as i64;
        let expected = [n / 6, n / 3, n / 2];
        for i in 0..3 {
            let diff = (counts[i] as i64 - expected[i] as i64).abs();
            assert!(
                diff <= tolerance,
                "validator {} got {} blocks, expected ~{} (±{})",
                i,
                counts[i],
                expected[i],
                tolerance
            );
        }
    }

    #[test]
    fn test_slot_leader_changes_with_parent_hash() {
        let snap = snapshot_with_stakes(&[1_000, 1_000, 1_000, 1_000]);
        let parent_a = hash::sha3_hash(b"a");
        let parent_b = hash::sha3_hash(b"b");
        let mut differ = 0u32;
        for height in 1..30 {
            let la = slot_leader(&snap, height, &parent_a);
            let lb = slot_leader(&snap, height, &parent_b);
            if la != lb {
                differ += 1;
            }
        }
        // With 4 equal-stake validators, ~75% of slots should pick a
        // different leader between two distinct parent hashes. Even
        // with sampling noise we expect well above half.
        assert!(
            differ >= 15,
            "expected most slots to differ, got {}",
            differ
        );
    }

    #[test]
    fn test_slot_leader_one_validator() {
        let snap = snapshot_with_stakes(&[5_000]);
        let parent = hash::sha3_hash(b"solo");
        for height in 1..20 {
            let leader = slot_leader(&snap, height, &parent).unwrap();
            assert_eq!(leader, vec![1u8; 20]);
            // Backups should still resolve to the same single validator
            // (the pool has nobody else to fall back to).
            let backup = slot_leader_at_rank(&snap, height, &parent, 5).unwrap();
            assert_eq!(backup, vec![1u8; 20]);
        }
    }

    #[test]
    fn test_backup_rank_progression() {
        let snap = snapshot_with_stakes(&[1_000, 1_000, 1_000]);
        let parent = hash::sha3_hash(b"backup");
        let h = 7u64;
        let r0 = slot_leader_at_rank(&snap, h, &parent, 0).unwrap();
        let r1 = slot_leader_at_rank(&snap, h, &parent, 1).unwrap();
        let r2 = slot_leader_at_rank(&snap, h, &parent, 2).unwrap();
        assert_ne!(r0, r1, "rank 0 and 1 must differ");
        assert_ne!(r1, r2, "rank 1 and 2 must differ");
        assert_ne!(r0, r2, "rank 0 and 2 must differ");
        // With N=3 validators, rank 3+ has nothing left and should
        // return the rank-2 fallback (last successfully chosen leader).
        let r3 = slot_leader_at_rank(&snap, h, &parent, 3).unwrap();
        assert_eq!(r3, r2);
    }

    #[test]
    fn test_allowed_backup_rank() {
        // Single validator: never allow backup.
        assert_eq!(allowed_backup_rank(100, 200, 1), 0);
        // Within first slot window: rank 0.
        assert_eq!(allowed_backup_rank(100, 100, 3), 0);
        assert_eq!(
            allowed_backup_rank(100, 100 + (BACKUP_LEADER_TIMEOUT_SECS as i64) - 1, 3),
            0
        );
        // Exactly at timeout: rank 1 becomes admissible.
        assert_eq!(
            allowed_backup_rank(100, 100 + BACKUP_LEADER_TIMEOUT_SECS as i64, 3),
            1
        );
        // Two timeouts elapsed: rank 2.
        assert_eq!(
            allowed_backup_rank(100, 100 + 2 * BACKUP_LEADER_TIMEOUT_SECS as i64, 3),
            2
        );
        // Beyond N-1 ranks: clamped.
        assert_eq!(
            allowed_backup_rank(100, 100 + 100 * BACKUP_LEADER_TIMEOUT_SECS as i64, 3),
            2
        );
    }
}
