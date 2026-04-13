use serde::{Deserialize, Serialize};

use crate::crypto::dilithium::{self, KeyPair, Signature};
use crate::crypto::hash;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransactionKind {
    Transfer,
    Stake,
    Coinbase,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub kind: TransactionKind,
    pub from: Vec<u8>,
    pub sender_public_key: Vec<u8>,
    pub to: Vec<u8>,
    pub amount: u64,
    pub fee: u64,
    pub nonce: u64,
    pub timestamp: i64,
    pub signature: Option<Signature>,
}

#[derive(Serialize)]
struct SignableTransaction<'a> {
    kind: &'a TransactionKind,
    from: &'a [u8],
    sender_public_key: &'a [u8],
    to: &'a [u8],
    amount: u64,
    fee: u64,
    nonce: u64,
    timestamp: i64,
}

impl Transaction {
    pub fn new(sender_public_key: Vec<u8>, to: Vec<u8>, amount: u64, fee: u64, nonce: u64) -> Self {
        Transaction {
            kind: TransactionKind::Transfer,
            from: hash::address_bytes_from_public_key(&sender_public_key),
            sender_public_key,
            to,
            amount,
            fee,
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            signature: None,
        }
    }

    pub fn stake(sender_public_key: Vec<u8>, amount: u64, fee: u64, nonce: u64) -> Self {
        Transaction {
            kind: TransactionKind::Stake,
            from: hash::address_bytes_from_public_key(&sender_public_key),
            sender_public_key,
            to: Vec::new(),
            amount,
            fee,
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            signature: None,
        }
    }

    pub fn coinbase(to: Vec<u8>, amount: u64) -> Self {
        Self::coinbase_with_timestamp(to, amount, chrono::Utc::now().timestamp())
    }

    pub fn coinbase_with_timestamp(to: Vec<u8>, amount: u64, timestamp: i64) -> Self {
        Transaction {
            kind: TransactionKind::Coinbase,
            from: vec![0; hash::ADDRESS_LEN],
            sender_public_key: Vec::new(),
            to,
            amount,
            fee: 0,
            nonce: 0,
            timestamp,
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
        let payload = SignableTransaction {
            kind: &self.kind,
            from: &self.from,
            sender_public_key: &self.sender_public_key,
            to: &self.to,
            amount: self.amount,
            fee: self.fee,
            nonce: self.nonce,
            timestamp: self.timestamp,
        };
        bincode::serialize(&payload).expect("failed to serialize transaction payload")
    }

    pub fn sign(&mut self, keypair: &KeyPair) {
        if self.is_coinbase() {
            return;
        }

        let data = self.signable_bytes();
        self.signature = Some(keypair.sign(&data));
    }

    pub fn verify_signature(&self) -> bool {
        if self.is_coinbase() {
            return true;
        }

        if self.from != hash::address_bytes_from_public_key(&self.sender_public_key) {
            return false;
        }

        match &self.signature {
            Some(sig) => {
                let data = self.signable_bytes();
                dilithium::verify(&data, sig, &self.sender_public_key)
            }
            None => false,
        }
    }

    pub fn is_coinbase(&self) -> bool {
        self.kind == TransactionKind::Coinbase
    }

    pub fn is_stake(&self) -> bool {
        self.kind == TransactionKind::Stake
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
            vec![1; hash::ADDRESS_LEN],
            1000,
            10,
            0,
        );
        tx.sign(&kp);
        assert!(tx.verify_signature());
    }

    #[test]
    fn test_coinbase_transaction() {
        let tx = Transaction::coinbase(vec![1; hash::ADDRESS_LEN], 50);
        assert!(tx.is_coinbase());
        assert!(tx.verify_signature());
    }

    #[test]
    fn test_stake_transaction() {
        let kp = KeyPair::generate();
        let mut tx = Transaction::stake(kp.public_key.clone(), 5000, 10, 0);
        tx.sign(&kp);
        assert!(tx.is_stake());
        assert!(tx.verify_signature());
        assert_eq!(tx.to.len(), 0);
    }

    #[test]
    fn test_rejects_forged_from_address() {
        let kp = KeyPair::generate();
        let mut tx = Transaction::new(
            kp.public_key.clone(),
            vec![1; hash::ADDRESS_LEN],
            1000,
            10,
            0,
        );
        tx.from = vec![9; hash::ADDRESS_LEN];
        tx.sign(&kp);
        assert!(!tx.verify_signature());
    }
}
