use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use argon2::Argon2;
use rand::RngCore;
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

/// Encrypted wallet file format stored on disk
#[derive(Serialize, Deserialize)]
struct EncryptedWallet {
    /// Argon2 salt (16 bytes, hex)
    salt: String,
    /// AES-GCM nonce (12 bytes, hex)
    nonce: String,
    /// Encrypted wallet data (hex)
    ciphertext: String,
    /// Version for future migration
    version: u32,
}

impl Wallet {
    pub fn new() -> Self {
        let keypair = KeyPair::generate();
        let address = Self::derive_address(&keypair.public_key);
        Wallet { keypair, address }
    }

    pub fn derive_address(public_key: &[u8]) -> String {
        hash::address_string_from_public_key(public_key)
    }

    pub fn derive_address_bytes(public_key: &[u8]) -> Vec<u8> {
        hash::address_bytes_from_public_key(public_key)
    }

    /// Save wallet encrypted with a password using AES-256-GCM + Argon2
    pub fn save_encrypted(&self, path: &str, password: &str) -> Result<(), WalletError> {
        let plaintext =
            serde_json::to_vec(self).map_err(|e| WalletError::Serialize(e.to_string()))?;

        // Derive key from password using Argon2
        let mut salt = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut salt);

        let mut key = [0u8; 32];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &salt, &mut key)
            .map_err(|e| WalletError::Encryption(e.to_string()))?;

        // Encrypt with AES-256-GCM
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|e| WalletError::Encryption(e.to_string()))?;

        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_ref())
            .map_err(|e| WalletError::Encryption(e.to_string()))?;

        let encrypted = EncryptedWallet {
            salt: hex::encode(salt),
            nonce: hex::encode(nonce_bytes),
            ciphertext: hex::encode(ciphertext),
            version: 1,
        };

        let json = serde_json::to_string_pretty(&encrypted)
            .map_err(|e| WalletError::Serialize(e.to_string()))?;
        fs::write(path, json)?;

        Ok(())
    }

    /// Load an encrypted wallet from disk
    pub fn load_encrypted(path: &str, password: &str) -> Result<Self, WalletError> {
        let data = fs::read_to_string(path)?;
        let encrypted: EncryptedWallet =
            serde_json::from_str(&data).map_err(|e| WalletError::Serialize(e.to_string()))?;

        let salt =
            hex::decode(&encrypted.salt).map_err(|e| WalletError::Serialize(e.to_string()))?;
        let nonce_bytes =
            hex::decode(&encrypted.nonce).map_err(|e| WalletError::Serialize(e.to_string()))?;
        let ciphertext = hex::decode(&encrypted.ciphertext)
            .map_err(|e| WalletError::Serialize(e.to_string()))?;

        // Derive key from password
        let mut key = [0u8; 32];
        Argon2::default()
            .hash_password_into(password.as_bytes(), &salt, &mut key)
            .map_err(|e| WalletError::Encryption(e.to_string()))?;

        // Decrypt
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|e| WalletError::Encryption(e.to_string()))?;
        let nonce = Nonce::from_slice(&nonce_bytes);

        let plaintext = cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|_| WalletError::WrongPassword)?;

        let wallet: Wallet = serde_json::from_slice(&plaintext)
            .map_err(|e| WalletError::Serialize(e.to_string()))?;

        Ok(wallet)
    }

    /// Legacy: Save wallet unencrypted (for backwards compat during migration)
    pub fn save(&self, path: &str) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(self).expect("failed to serialize wallet");
        fs::write(path, json)
    }

    /// Legacy: Load unencrypted wallet
    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let data = fs::read_to_string(path)?;
        let wallet: Wallet = serde_json::from_str(&data)?;
        Ok(wallet)
    }

    /// Try encrypted load first, fallback to legacy unencrypted
    pub fn load_auto(path: &str, password: &str) -> Result<Self, WalletError> {
        // Try encrypted first
        match Self::load_encrypted(path, password) {
            Ok(w) => Ok(w),
            Err(WalletError::WrongPassword) => Err(WalletError::WrongPassword),
            Err(_) => {
                // Try legacy unencrypted format
                match Self::load(path) {
                    Ok(w) => {
                        // Auto-migrate: re-save as encrypted
                        eprintln!("Migrating wallet to encrypted format...");
                        w.save_encrypted(path, password)?;
                        Ok(w)
                    }
                    Err(e) => Err(WalletError::Io(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e.to_string(),
                    ))),
                }
            }
        }
    }

    pub fn exists(path: &str) -> bool {
        Path::new(path).exists()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WalletError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialize(String),
    #[error("encryption error: {0}")]
    Encryption(String),
    #[error("wrong password")]
    WrongPassword,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_wallet() {
        let wallet = Wallet::new();
        assert!(wallet.address.starts_with("CUR"));
        assert_eq!(wallet.address.len(), 43);
    }

    #[test]
    fn test_deterministic_address() {
        let wallet = Wallet::new();
        let addr1 = Wallet::derive_address(&wallet.keypair.public_key);
        let addr2 = Wallet::derive_address(&wallet.keypair.public_key);
        assert_eq!(addr1, addr2);
        assert_eq!(
            Wallet::derive_address_bytes(&wallet.keypair.public_key).len(),
            crate::crypto::hash::ADDRESS_LEN
        );
    }

    #[test]
    fn test_encrypted_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_wallet.json");
        let path_str = path.to_str().unwrap();

        let wallet = Wallet::new();
        wallet.save_encrypted(path_str, "mypassword123").unwrap();

        let loaded = Wallet::load_encrypted(path_str, "mypassword123").unwrap();
        assert_eq!(wallet.address, loaded.address);
        assert_eq!(wallet.keypair.public_key, loaded.keypair.public_key);
    }

    #[test]
    fn test_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_wallet.json");
        let path_str = path.to_str().unwrap();

        let wallet = Wallet::new();
        wallet.save_encrypted(path_str, "correct").unwrap();

        let result = Wallet::load_encrypted(path_str, "wrong");
        assert!(matches!(result, Err(WalletError::WrongPassword)));
    }

    #[test]
    fn test_auto_migrate_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy_wallet.json");
        let path_str = path.to_str().unwrap();

        let wallet = Wallet::new();
        wallet.save(path_str).unwrap(); // Save as legacy

        let loaded = Wallet::load_auto(path_str, "newpass").unwrap();
        assert_eq!(wallet.address, loaded.address);

        // Should now be encrypted
        let reloaded = Wallet::load_encrypted(path_str, "newpass").unwrap();
        assert_eq!(wallet.address, reloaded.address);
    }
}
