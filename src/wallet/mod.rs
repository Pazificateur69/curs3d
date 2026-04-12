use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::crypto::dilithium::KeyPair;
use crate::crypto::hash;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Wallet {
    pub keypair: KeyPair,
    pub address: String,
}

impl Wallet {
    pub fn new() -> Self {
        let keypair = KeyPair::generate();
        let address = Self::derive_address(&keypair.public_key);
        Wallet { keypair, address }
    }

    pub fn derive_address(public_key: &[u8]) -> String {
        let hash = hash::sha3_hash(public_key);
        format!("CUR{}", hex::encode(&hash[..20]))
    }

    pub fn save(&self, path: &str) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(self).expect("failed to serialize wallet");
        fs::write(path, json)
    }

    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let data = fs::read_to_string(path)?;
        let wallet: Wallet = serde_json::from_str(&data)?;
        Ok(wallet)
    }

    pub fn exists(path: &str) -> bool {
        Path::new(path).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_wallet() {
        let wallet = Wallet::new();
        assert!(wallet.address.starts_with("CUR"));
        assert_eq!(wallet.address.len(), 43); // "CUR" + 40 hex chars
    }

    #[test]
    fn test_deterministic_address() {
        let wallet = Wallet::new();
        let addr1 = Wallet::derive_address(&wallet.keypair.public_key);
        let addr2 = Wallet::derive_address(&wallet.keypair.public_key);
        assert_eq!(addr1, addr2);
    }
}
