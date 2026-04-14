use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::core::block::{Block, BlockHeader};
use crate::core::chain::AccountState;
use crate::crypto::dilithium::{self, Signature};

/// Percentage of stake slashed on equivocation (33%)
pub const EQUIVOCATION_SLASH_PERCENT: u64 = 33;

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
    pub fn new(
        block_hash: Vec<u8>,
        block_height: u64,
        epoch: u64,
        keypair: &crate::crypto::dilithium::KeyPair,
    ) -> Self {
        let mut data = block_hash.clone();
        data.extend_from_slice(&block_height.to_le_bytes());
        data.extend_from_slice(&epoch.to_le_bytes());
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
        let mut data = self.block_hash.clone();
        data.extend_from_slice(&self.block_height.to_le_bytes());
        data.extend_from_slice(&self.epoch.to_le_bytes());
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
            finalized_height: 0,
            finalized_hash: Vec::new(),
        }
    }

    /// Load from persisted state
    pub fn with_finalized(height: u64, hash: Vec<u8>) -> Self {
        FinalityTracker {
            votes: HashMap::new(),
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

        // Deduplicate by voter
        let block_hash = vote.block_hash.clone();
        let block_height = vote.block_height;

        {
            let votes = self.votes.entry(block_hash.clone()).or_default();
            if votes
                .iter()
                .any(|v| v.voter_public_key == vote.voter_public_key)
            {
                return None; // Already voted
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

        let mut seed = Vec::new();
        seed.extend_from_slice(&block_height.to_le_bytes());
        seed.extend_from_slice(prev_hash);
        let hash = crate::crypto::hash::sha3_hash(&seed);
        let mut selector = u64::from_le_bytes(hash[..8].try_into().unwrap()) % total_stake;

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

        let mut seed = Vec::new();
        seed.extend_from_slice(&block_height.to_le_bytes());
        seed.extend_from_slice(prev_hash);
        let hash = crate::crypto::hash::sha3_hash(&seed);
        let mut selector = u64::from_le_bytes(hash[..8].try_into().unwrap()) % total_stake;

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
}
