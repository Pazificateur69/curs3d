use std::collections::HashMap;

use crate::core::block::Block;
use crate::core::transaction::Transaction;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ChainError {
    #[error("invalid block height: expected {expected}, got {got}")]
    InvalidHeight { expected: u64, got: u64 },
    #[error("invalid previous hash")]
    InvalidPrevHash,
    #[error("invalid merkle root")]
    InvalidMerkleRoot,
    #[error("invalid transaction signature")]
    InvalidSignature,
    #[error("insufficient balance: {address} has {balance}, needs {needed}")]
    InsufficientBalance {
        address: String,
        balance: u64,
        needed: u64,
    },
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
    #[error("duplicate transaction")]
    DuplicateTransaction,
}

#[derive(Clone, Debug)]
pub struct AccountState {
    pub balance: u64,
    pub nonce: u64,
}

pub struct Blockchain {
    pub blocks: Vec<Block>,
    pub accounts: HashMap<Vec<u8>, AccountState>,
    pub pending_transactions: Vec<Transaction>,
    pub block_reward: u64,
}

impl Blockchain {
    pub fn new() -> Self {
        let genesis = Block::genesis();
        Blockchain {
            blocks: vec![genesis],
            accounts: HashMap::new(),
            pending_transactions: Vec::new(),
            block_reward: 50_000_000, // 50 CURS3D (in microtokens)
        }
    }

    pub fn height(&self) -> u64 {
        self.blocks.len() as u64 - 1
    }

    pub fn latest_block(&self) -> &Block {
        self.blocks.last().expect("chain must have at least genesis")
    }

    pub fn get_balance(&self, address: &[u8]) -> u64 {
        self.accounts
            .get(address)
            .map(|a| a.balance)
            .unwrap_or(0)
    }

    pub fn get_nonce(&self, address: &[u8]) -> u64 {
        self.accounts
            .get(address)
            .map(|a| a.nonce)
            .unwrap_or(0)
    }

    pub fn add_transaction(&mut self, tx: Transaction) -> Result<(), ChainError> {
        if !tx.is_coinbase() {
            if !tx.verify_signature() {
                return Err(ChainError::InvalidSignature);
            }

            let account = self.accounts.get(&tx.from);
            let balance = account.map(|a| a.balance).unwrap_or(0);
            let needed = tx.amount + tx.fee;
            if balance < needed {
                return Err(ChainError::InsufficientBalance {
                    address: hex::encode(&tx.from[..8]),
                    balance,
                    needed,
                });
            }

            let expected_nonce = account.map(|a| a.nonce).unwrap_or(0);
            if tx.nonce != expected_nonce {
                return Err(ChainError::InvalidNonce {
                    expected: expected_nonce,
                    got: tx.nonce,
                });
            }
        }

        self.pending_transactions.push(tx);
        Ok(())
    }

    pub fn create_block(&mut self, validator: Vec<u8>) -> Result<Block, ChainError> {
        let prev_block = self.latest_block();
        let height = prev_block.header.height + 1;
        let prev_hash = prev_block.hash.clone();

        let coinbase = Transaction::coinbase(validator.clone(), self.block_reward);
        let mut block_txs = vec![coinbase];
        block_txs.extend(self.pending_transactions.drain(..));

        let block = Block::new(height, prev_hash, block_txs, validator);
        Ok(block)
    }

    pub fn add_block(&mut self, block: Block) -> Result<(), ChainError> {
        let prev = self.latest_block();

        if block.header.height != prev.header.height + 1 {
            return Err(ChainError::InvalidHeight {
                expected: prev.header.height + 1,
                got: block.header.height,
            });
        }

        if block.header.prev_hash != prev.hash {
            return Err(ChainError::InvalidPrevHash);
        }

        if !block.verify_merkle_root() {
            return Err(ChainError::InvalidMerkleRoot);
        }

        for tx in &block.transactions {
            if !tx.is_coinbase() && !tx.verify_signature() {
                return Err(ChainError::InvalidSignature);
            }
        }

        self.apply_transactions(&block);
        self.blocks.push(block);
        Ok(())
    }

    fn apply_transactions(&mut self, block: &Block) {
        for tx in &block.transactions {
            if tx.is_coinbase() {
                let account = self.accounts.entry(tx.to.clone()).or_insert(AccountState {
                    balance: 0,
                    nonce: 0,
                });
                account.balance += tx.amount;
                continue;
            }

            // Debit sender
            if let Some(sender) = self.accounts.get_mut(&tx.from) {
                sender.balance = sender.balance.saturating_sub(tx.amount + tx.fee);
                sender.nonce += 1;
            }

            // Credit receiver
            let receiver = self.accounts.entry(tx.to.clone()).or_insert(AccountState {
                balance: 0,
                nonce: 0,
            });
            receiver.balance += tx.amount;
        }
    }

    pub fn is_valid(&self) -> bool {
        for i in 1..self.blocks.len() {
            let block = &self.blocks[i];
            let prev = &self.blocks[i - 1];

            if block.header.prev_hash != prev.hash {
                return false;
            }
            if !block.verify_merkle_root() {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::dilithium::KeyPair;

    #[test]
    fn test_new_blockchain() {
        let chain = Blockchain::new();
        assert_eq!(chain.height(), 0);
        assert!(chain.is_valid());
    }

    #[test]
    fn test_create_and_add_block() {
        let mut chain = Blockchain::new();
        let validator = vec![1; 32];
        let block = chain.create_block(validator).unwrap();
        chain.add_block(block).unwrap();
        assert_eq!(chain.height(), 1);
        assert!(chain.is_valid());
    }

    #[test]
    fn test_transaction_flow() {
        let mut chain = Blockchain::new();
        let validator_kp = KeyPair::generate();

        // Mine a block to get funds
        let block = chain.create_block(validator_kp.public_key.clone()).unwrap();
        chain.add_block(block).unwrap();

        // Create and sign a transaction
        let recipient = KeyPair::generate();
        let mut tx = Transaction::new(
            validator_kp.public_key.clone(),
            recipient.public_key.clone(),
            1000,
            10,
            0,
        );
        tx.sign(&validator_kp);
        chain.add_transaction(tx).unwrap();

        // Mine block with transaction
        let block = chain.create_block(validator_kp.public_key.clone()).unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(chain.get_balance(&recipient.public_key), 1000);
        assert!(chain.is_valid());
    }
}
