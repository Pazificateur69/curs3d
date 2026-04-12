use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Validator {
    pub public_key: Vec<u8>,
    pub stake: u64,
    pub is_active: bool,
}

pub struct ProofOfStake {
    pub validators: HashMap<Vec<u8>, Validator>,
    pub minimum_stake: u64,
    pub epoch_length: u64,
    pub current_epoch: u64,
}

impl ProofOfStake {
    pub fn new(minimum_stake: u64, epoch_length: u64) -> Self {
        ProofOfStake {
            validators: HashMap::new(),
            minimum_stake,
            epoch_length,
            current_epoch: 0,
        }
    }

    pub fn register_validator(&mut self, public_key: Vec<u8>, stake: u64) -> Result<(), String> {
        if stake < self.minimum_stake {
            return Err(format!(
                "stake {} below minimum {}",
                stake, self.minimum_stake
            ));
        }

        self.validators.insert(
            public_key.clone(),
            Validator {
                public_key,
                stake,
                is_active: true,
            },
        );
        Ok(())
    }

    pub fn remove_validator(&mut self, public_key: &[u8]) -> Option<Validator> {
        self.validators.remove(public_key)
    }

    pub fn select_validator(&self, block_height: u64, prev_hash: &[u8]) -> Option<Vec<u8>> {
        let active: Vec<&Validator> = self
            .validators
            .values()
            .filter(|v| v.is_active)
            .collect();

        if active.is_empty() {
            return None;
        }

        let total_stake: u64 = active.iter().map(|v| v.stake).sum();
        if total_stake == 0 {
            return None;
        }

        // Deterministic selection weighted by stake
        let mut seed = Vec::new();
        seed.extend_from_slice(&block_height.to_le_bytes());
        seed.extend_from_slice(prev_hash);
        let hash = crate::crypto::hash::sha3_hash(&seed);

        let mut selector = u64::from_le_bytes(hash[..8].try_into().unwrap()) % total_stake;

        let mut sorted: Vec<&Validator> = active;
        sorted.sort_by(|a, b| a.public_key.cmp(&b.public_key));

        for validator in sorted {
            if selector < validator.stake {
                return Some(validator.public_key.clone());
            }
            selector -= validator.stake;
        }

        None
    }

    pub fn slash(&mut self, public_key: &[u8], penalty_percent: u64) {
        if let Some(validator) = self.validators.get_mut(public_key) {
            let penalty = validator.stake * penalty_percent / 100;
            validator.stake = validator.stake.saturating_sub(penalty);
            if validator.stake < self.minimum_stake {
                validator.is_active = false;
            }
        }
    }

    pub fn total_staked(&self) -> u64 {
        self.validators.values().map(|v| v.stake).sum()
    }

    pub fn active_validator_count(&self) -> usize {
        self.validators.values().filter(|v| v.is_active).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_validator() {
        let mut pos = ProofOfStake::new(1000, 100);
        pos.register_validator(vec![1; 32], 5000).unwrap();
        assert_eq!(pos.active_validator_count(), 1);
        assert_eq!(pos.total_staked(), 5000);
    }

    #[test]
    fn test_minimum_stake() {
        let mut pos = ProofOfStake::new(1000, 100);
        let result = pos.register_validator(vec![1; 32], 500);
        assert!(result.is_err());
    }

    #[test]
    fn test_validator_selection() {
        let mut pos = ProofOfStake::new(1000, 100);
        pos.register_validator(vec![1; 32], 5000).unwrap();
        pos.register_validator(vec![2; 32], 3000).unwrap();

        let selected = pos.select_validator(1, &[0; 32]);
        assert!(selected.is_some());
    }

    #[test]
    fn test_slashing() {
        let mut pos = ProofOfStake::new(1000, 100);
        pos.register_validator(vec![1; 32], 5000).unwrap();
        pos.slash(&vec![1; 32], 50);
        assert_eq!(pos.validators[&vec![1; 32]].stake, 2500);
    }
}
