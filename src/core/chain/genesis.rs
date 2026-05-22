//! Genesis-config helpers — derive the initial account map from a
//! `GenesisConfig`, parse hex-encoded validator pubkeys, and compute
//! epoch boundary heights. Extracted from `chain/mod.rs` in #29.

use std::collections::HashMap;

use super::{AccountState, Blockchain, ChainError, GenesisConfig, PendingUnstake};
use crate::crypto::hash;

impl Blockchain {
    pub(super) fn accounts_from_genesis(
        genesis_config: &GenesisConfig,
    ) -> Result<HashMap<Vec<u8>, AccountState>, ChainError> {
        let mut accounts = HashMap::new();

        for allocation in &genesis_config.allocations {
            let public_key = Self::decode_public_key_hex(&allocation.public_key)?;
            let address = hash::address_bytes_from_public_key(&public_key);
            if accounts.contains_key(&address) {
                return Err(ChainError::InvalidGenesis(
                    "duplicate account in genesis".to_string(),
                ));
            }

            accounts.insert(
                address,
                AccountState {
                    balance: allocation.balance,
                    nonce: 0,
                    staked_balance: allocation.staked_balance,
                    pending_unstakes: Vec::<PendingUnstake>::new(),
                    validator_active_from_height: if allocation.staked_balance
                        >= genesis_config.minimum_stake
                    {
                        1
                    } else {
                        0
                    },
                    jailed_until_height: 0,
                    public_key: Some(public_key),
                },
            );
        }

        Ok(accounts)
    }

    pub(super) fn decode_public_key_hex(value: &str) -> Result<Vec<u8>, ChainError> {
        let raw = value.strip_prefix("0x").unwrap_or(value);
        hex::decode(raw).map_err(|_| {
            ChainError::InvalidGenesis("invalid public_key hex in genesis".to_string())
        })
    }

    pub(super) fn next_epoch_start_height_for(current_height: u64, epoch_length: u64) -> u64 {
        let epoch_length = epoch_length.max(1);
        current_height
            .saturating_div(epoch_length)
            .saturating_add(1)
            .saturating_mul(epoch_length)
    }
}
