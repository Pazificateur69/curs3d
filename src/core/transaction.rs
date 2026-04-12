use serde::{Deserialize, Serialize};

use crate::crypto::dilithium::{self, KeyPair, Signature};
use crate::crypto::hash;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub from: Vec<u8>,
    pub to: Vec<u8>,
    pub amount: u64,
    pub fee: u64,
    pub nonce: u64,
    pub timestamp: i64,
    pub signature: Option<Signature>,
}

impl Transaction {
    pub fn new(from: Vec<u8>, to: Vec<u8>, amount: u64, fee: u64, nonce: u64) -> Self {
        Transaction {
            from,
            to,
            amount,
            fee,
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            signature: None,
        }
    }

    pub fn coinbase(to: Vec<u8>, amount: u64) -> Self {
        Transaction {
            from: vec![0; 32],
            to,
            amount,
            fee: 0,
            nonce: 0,
            timestamp: chrono::Utc::now().timestamp(),
            signature: None,
        }
    }

    pub fn hash(&self) -> Vec<u8> {
        let data = self.signable_bytes();
        hash::sha3_hash(&data)
    }

    pub fn hash_hex(&self) -> String {
        hex::encode(self.hash())
    }

    fn signable_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.from);
        bytes.extend_from_slice(&self.to);
        bytes.extend_from_slice(&self.amount.to_le_bytes());
        bytes.extend_from_slice(&self.fee.to_le_bytes());
        bytes.extend_from_slice(&self.nonce.to_le_bytes());
        bytes.extend_from_slice(&self.timestamp.to_le_bytes());
        bytes
    }

    pub fn sign(&mut self, keypair: &KeyPair) {
        let data = self.signable_bytes();
        self.signature = Some(keypair.sign(&data));
    }

    pub fn verify_signature(&self) -> bool {
        match &self.signature {
            Some(sig) => {
                let data = self.signable_bytes();
                dilithium::verify(&data, sig, &self.from)
            }
            None => false,
        }
    }

    pub fn is_coinbase(&self) -> bool {
        self.from.iter().all(|&b| b == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_and_verify_transaction() {
        let kp = KeyPair::generate();
        let mut tx = Transaction::new(
            kp.public_key.clone(),
            vec![1; 32],
            1000,
            10,
            0,
        );
        tx.sign(&kp);
        assert!(tx.verify_signature());
    }

    #[test]
    fn test_coinbase_transaction() {
        let tx = Transaction::coinbase(vec![1; 32], 50);
        assert!(tx.is_coinbase());
    }
}
