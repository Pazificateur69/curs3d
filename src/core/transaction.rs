use serde::{Deserialize, Serialize};

use crate::crypto::dilithium::{self, KeyPair, Signature};
use crate::crypto::hash;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransactionKind {
    Transfer,
    Stake,
    Unstake,
    Coinbase,
    DeployContract,
    CallContract,
    // CUR-20 token operations (added at end for bincode compat)
    DeployToken,
    TokenTransfer,
    TokenApprove,
    TokenTransferFrom,
    // Governance operations
    SubmitProposal,
    GovernanceVote,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub chain_id: String,
    pub kind: TransactionKind,
    pub from: Vec<u8>,
    pub sender_public_key: Vec<u8>,
    pub to: Vec<u8>,
    pub amount: u64,
    #[serde(default)]
    pub fee: u64,
    #[serde(default)]
    pub max_fee_per_gas: u64,
    #[serde(default)]
    pub max_priority_fee_per_gas: u64,
    pub nonce: u64,
    pub timestamp: i64,
    pub signature: Option<Signature>,
    #[serde(default)]
    pub gas_limit: u64,
    #[serde(default)]
    pub data: Vec<u8>,
}

#[derive(Serialize)]
struct SignableTransaction<'a> {
    chain_id: &'a str,
    kind: &'a TransactionKind,
    from: &'a [u8],
    sender_public_key: &'a [u8],
    to: &'a [u8],
    amount: u64,
    fee: u64,
    max_fee_per_gas: u64,
    max_priority_fee_per_gas: u64,
    nonce: u64,
    timestamp: i64,
    gas_limit: u64,
    data: &'a [u8],
}

impl Transaction {
    fn canonical_max_fee_per_gas(&self) -> u64 {
        self.max_fee_per_gas.max(self.fee)
    }

    fn canonical_max_priority_fee_per_gas(&self) -> u64 {
        let max_fee = self.canonical_max_fee_per_gas();
        if self.max_priority_fee_per_gas == 0 {
            self.fee.min(max_fee)
        } else {
            self.max_priority_fee_per_gas.min(max_fee)
        }
    }

    pub fn intrinsic_gas(&self) -> u64 {
        let payload_bytes = match self.kind {
            TransactionKind::Transfer => self.data.len(),
            TransactionKind::Stake | TransactionKind::Unstake | TransactionKind::Coinbase => 0,
            TransactionKind::DeployContract => self.to.len().saturating_add(self.data.len()),
            TransactionKind::CallContract => self.data.len(),
            TransactionKind::DeployToken
            | TransactionKind::TokenTransfer
            | TransactionKind::TokenApprove
            | TransactionKind::TokenTransferFrom => self.data.len(),
            TransactionKind::SubmitProposal | TransactionKind::GovernanceVote => self.data.len(),
        } as u64;

        let kind_gas = match self.kind {
            TransactionKind::Transfer
            | TransactionKind::Stake
            | TransactionKind::Unstake
            | TransactionKind::Coinbase => crate::vm::gas::GAS_BASE_TX,
            TransactionKind::DeployContract => {
                crate::vm::gas::GAS_BASE_TX.saturating_add(crate::vm::gas::GAS_DEPLOY)
            }
            TransactionKind::CallContract => {
                crate::vm::gas::GAS_BASE_TX.saturating_add(crate::vm::gas::GAS_CALL)
            }
            TransactionKind::DeployToken => crate::vm::gas::GAS_BASE_TX,
            TransactionKind::TokenTransfer
            | TransactionKind::TokenApprove
            | TransactionKind::TokenTransferFrom => crate::vm::gas::GAS_BASE_TX,
            TransactionKind::SubmitProposal | TransactionKind::GovernanceVote => {
                crate::vm::gas::GAS_BASE_TX
            }
        };

        kind_gas.saturating_add(payload_bytes.saturating_mul(crate::vm::gas::GAS_PER_BYTE))
    }

    pub fn estimated_gas_for_admission(&self) -> u64 {
        match self.kind {
            TransactionKind::DeployContract | TransactionKind::CallContract => {
                self.gas_limit.max(self.intrinsic_gas())
            }
            _ => self.intrinsic_gas(),
        }
    }

    pub fn effective_gas_limit(&self) -> u64 {
        self.estimated_gas_for_admission()
    }

    pub fn max_fee_per_gas(&self) -> u64 {
        self.canonical_max_fee_per_gas()
    }

    pub fn max_priority_fee_per_gas(&self) -> u64 {
        self.canonical_max_priority_fee_per_gas()
    }

    pub fn total_fee_cap(&self) -> u64 {
        self.effective_gas_limit()
            .saturating_mul(self.max_fee_per_gas())
    }

    pub fn effective_gas_price(&self, base_fee_per_gas: u64) -> Option<u64> {
        let max_fee = self.max_fee_per_gas();
        if max_fee < base_fee_per_gas {
            return None;
        }
        let priority = self
            .max_priority_fee_per_gas()
            .min(max_fee.saturating_sub(base_fee_per_gas));
        Some(base_fee_per_gas.saturating_add(priority))
    }

    pub fn priority_fee_per_gas(&self, base_fee_per_gas: u64) -> Option<u64> {
        self.effective_gas_price(base_fee_per_gas)
            .map(|effective| effective.saturating_sub(base_fee_per_gas))
    }

    #[allow(dead_code)]
    pub fn with_fee_caps(mut self, max_fee_per_gas: u64, max_priority_fee_per_gas: u64) -> Self {
        self.fee = max_fee_per_gas;
        self.max_fee_per_gas = max_fee_per_gas;
        self.max_priority_fee_per_gas = max_priority_fee_per_gas.min(max_fee_per_gas);
        self
    }

    pub fn new(
        chain_id: &str,
        sender_public_key: Vec<u8>,
        to: Vec<u8>,
        amount: u64,
        fee: u64,
        nonce: u64,
    ) -> Self {
        Transaction {
            chain_id: chain_id.to_string(),
            kind: TransactionKind::Transfer,
            from: hash::address_bytes_from_public_key(&sender_public_key),
            sender_public_key,
            to,
            amount,
            fee,
            max_fee_per_gas: fee,
            max_priority_fee_per_gas: fee,
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            signature: None,
            gas_limit: 0,
            data: Vec::new(),
        }
    }

    pub fn stake(
        chain_id: &str,
        sender_public_key: Vec<u8>,
        amount: u64,
        fee: u64,
        nonce: u64,
    ) -> Self {
        Transaction {
            chain_id: chain_id.to_string(),
            kind: TransactionKind::Stake,
            from: hash::address_bytes_from_public_key(&sender_public_key),
            sender_public_key,
            to: Vec::new(),
            amount,
            fee,
            max_fee_per_gas: fee,
            max_priority_fee_per_gas: fee,
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            signature: None,
            gas_limit: 0,
            data: Vec::new(),
        }
    }

    pub fn coinbase(chain_id: &str, to: Vec<u8>, amount: u64) -> Self {
        Self::coinbase_with_timestamp(chain_id, to, amount, chrono::Utc::now().timestamp())
    }

    pub fn coinbase_with_timestamp(
        chain_id: &str,
        to: Vec<u8>,
        amount: u64,
        timestamp: i64,
    ) -> Self {
        Transaction {
            chain_id: chain_id.to_string(),
            kind: TransactionKind::Coinbase,
            from: vec![0; hash::ADDRESS_LEN],
            sender_public_key: Vec::new(),
            to,
            amount,
            fee: 0,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            nonce: 0,
            timestamp,
            signature: None,
            gas_limit: 0,
            data: Vec::new(),
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
            chain_id: &self.chain_id,
            kind: &self.kind,
            from: &self.from,
            sender_public_key: &self.sender_public_key,
            to: &self.to,
            amount: self.amount,
            fee: self.fee,
            max_fee_per_gas: self.max_fee_per_gas(),
            max_priority_fee_per_gas: self.max_priority_fee_per_gas(),
            nonce: self.nonce,
            timestamp: self.timestamp,
            gas_limit: self.gas_limit,
            data: &self.data,
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

    pub fn is_unstake(&self) -> bool {
        self.kind == TransactionKind::Unstake
    }

    #[allow(dead_code)]
    pub fn deploy_contract(
        chain_id: &str,
        sender_public_key: Vec<u8>,
        code: Vec<u8>,
        gas_limit: u64,
        fee: u64,
        nonce: u64,
    ) -> Self {
        Transaction {
            chain_id: chain_id.to_string(),
            kind: TransactionKind::DeployContract,
            from: hash::address_bytes_from_public_key(&sender_public_key),
            sender_public_key,
            to: code,
            amount: 0,
            fee,
            max_fee_per_gas: fee,
            max_priority_fee_per_gas: fee,
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            signature: None,
            gas_limit,
            data: Vec::new(),
        }
    }

    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn call_contract(
        chain_id: &str,
        sender_public_key: Vec<u8>,
        contract_addr: Vec<u8>,
        input_data: Vec<u8>,
        value: u64,
        gas_limit: u64,
        fee: u64,
        nonce: u64,
    ) -> Self {
        Transaction {
            chain_id: chain_id.to_string(),
            kind: TransactionKind::CallContract,
            from: hash::address_bytes_from_public_key(&sender_public_key),
            sender_public_key,
            to: contract_addr,
            amount: value,
            fee,
            max_fee_per_gas: fee,
            max_priority_fee_per_gas: fee,
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            signature: None,
            gas_limit,
            data: input_data,
        }
    }

    #[allow(dead_code)]
    pub fn is_deploy_contract(&self) -> bool {
        self.kind == TransactionKind::DeployContract
    }

    #[allow(dead_code)]
    pub fn is_call_contract(&self) -> bool {
        self.kind == TransactionKind::CallContract
    }

    #[allow(dead_code)]
    pub fn unstake(
        chain_id: &str,
        sender_public_key: Vec<u8>,
        amount: u64,
        fee: u64,
        nonce: u64,
    ) -> Self {
        Transaction {
            chain_id: chain_id.to_string(),
            kind: TransactionKind::Unstake,
            from: hash::address_bytes_from_public_key(&sender_public_key),
            sender_public_key,
            to: Vec::new(),
            amount,
            fee,
            max_fee_per_gas: fee,
            max_priority_fee_per_gas: fee,
            nonce,
            timestamp: chrono::Utc::now().timestamp(),
            signature: None,
            gas_limit: 0,
            data: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_and_verify_transaction() {
        let kp = KeyPair::generate();
        let mut tx = Transaction::new(
            "test-chain",
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
        let tx = Transaction::coinbase("test-chain", vec![1; hash::ADDRESS_LEN], 50);
        assert!(tx.is_coinbase());
        assert!(tx.verify_signature());
    }

    #[test]
    fn test_stake_transaction() {
        let kp = KeyPair::generate();
        let mut tx = Transaction::stake("test-chain", kp.public_key.clone(), 5000, 10, 0);
        tx.sign(&kp);
        assert!(tx.is_stake());
        assert!(tx.verify_signature());
        assert_eq!(tx.to.len(), 0);
    }

    #[test]
    fn test_unstake_transaction() {
        let kp = KeyPair::generate();
        let mut tx = Transaction::unstake("test-chain", kp.public_key.clone(), 5000, 10, 0);
        tx.sign(&kp);
        assert!(tx.is_unstake());
        assert!(tx.verify_signature());
        assert_eq!(tx.to.len(), 0);
    }

    #[test]
    fn test_rejects_forged_from_address() {
        let kp = KeyPair::generate();
        let mut tx = Transaction::new(
            "test-chain",
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
