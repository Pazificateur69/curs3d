use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ─── CUR-20 Token Standard ─────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CUR20Token {
    pub contract_address: Vec<u8>,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub total_supply: u64,
    pub creator: Vec<u8>,
    pub created_at_height: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeployTokenParams {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub total_supply: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenTransferParams {
    pub token_address: Vec<u8>,
    pub recipient: Vec<u8>,
    pub amount: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenApproveParams {
    pub token_address: Vec<u8>,
    pub spender: Vec<u8>,
    pub amount: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenTransferFromParams {
    pub token_address: Vec<u8>,
    pub from: Vec<u8>,
    pub recipient: Vec<u8>,
    pub amount: u64,
}

/// Key for balance: (token_address, owner_address)
pub type BalanceKey = (Vec<u8>, Vec<u8>);

/// Key for allowance: (token_address, owner_address, spender_address)
pub type AllowanceKey = (Vec<u8>, Vec<u8>, Vec<u8>);

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenRegistry {
    pub tokens: HashMap<Vec<u8>, CUR20Token>,
    pub balances: HashMap<BalanceKey, u64>,
    pub allowances: HashMap<AllowanceKey, u64>,
}

impl TokenRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn deploy_token(
        &mut self,
        deployer: &[u8],
        nonce: u64,
        params: &DeployTokenParams,
        height: u64,
    ) -> Result<Vec<u8>, TokenError> {
        if params.name.is_empty() || params.name.len() > 64 {
            return Err(TokenError::InvalidName);
        }
        if params.symbol.is_empty() || params.symbol.len() > 12 {
            return Err(TokenError::InvalidSymbol);
        }
        if params.decimals > 18 {
            return Err(TokenError::InvalidDecimals);
        }
        if params.total_supply == 0 {
            return Err(TokenError::InvalidSupply);
        }

        // Derive deterministic token address: SHA-3(deployer || nonce || "token")
        let mut seed = deployer.to_vec();
        seed.extend_from_slice(&nonce.to_le_bytes());
        seed.extend_from_slice(b"cur20");
        let address = crate::crypto::hash::address_bytes_from_data(&seed);

        if self.tokens.contains_key(&address) {
            return Err(TokenError::AlreadyExists);
        }

        let token = CUR20Token {
            contract_address: address.clone(),
            name: params.name.clone(),
            symbol: params.symbol.clone(),
            decimals: params.decimals,
            total_supply: params.total_supply,
            creator: deployer.to_vec(),
            created_at_height: height,
        };

        self.tokens.insert(address.clone(), token);
        // Assign total supply to deployer
        self.balances
            .insert((address.clone(), deployer.to_vec()), params.total_supply);

        Ok(address)
    }

    pub fn transfer(
        &mut self,
        token_address: &[u8],
        from: &[u8],
        to: &[u8],
        amount: u64,
    ) -> Result<(), TokenError> {
        if !self.tokens.contains_key(token_address) {
            return Err(TokenError::TokenNotFound);
        }
        if from == to {
            return Err(TokenError::SelfTransfer);
        }
        if amount == 0 {
            return Err(TokenError::ZeroAmount);
        }

        let from_key = (token_address.to_vec(), from.to_vec());
        let from_balance = self.balances.get(&from_key).copied().unwrap_or(0);
        if from_balance < amount {
            return Err(TokenError::InsufficientBalance);
        }

        self.balances.insert(from_key, from_balance - amount);

        let to_key = (token_address.to_vec(), to.to_vec());
        let to_balance = self.balances.get(&to_key).copied().unwrap_or(0);
        self.balances.insert(to_key, to_balance + amount);

        Ok(())
    }

    pub fn approve(
        &mut self,
        token_address: &[u8],
        owner: &[u8],
        spender: &[u8],
        amount: u64,
    ) -> Result<(), TokenError> {
        if !self.tokens.contains_key(token_address) {
            return Err(TokenError::TokenNotFound);
        }

        let key = (token_address.to_vec(), owner.to_vec(), spender.to_vec());
        self.allowances.insert(key, amount);

        Ok(())
    }

    pub fn transfer_from(
        &mut self,
        token_address: &[u8],
        spender: &[u8],
        from: &[u8],
        to: &[u8],
        amount: u64,
    ) -> Result<(), TokenError> {
        if !self.tokens.contains_key(token_address) {
            return Err(TokenError::TokenNotFound);
        }
        if amount == 0 {
            return Err(TokenError::ZeroAmount);
        }

        // Check allowance
        let allowance_key = (token_address.to_vec(), from.to_vec(), spender.to_vec());
        let allowance = self.allowances.get(&allowance_key).copied().unwrap_or(0);
        if allowance < amount {
            return Err(TokenError::InsufficientAllowance);
        }

        // Check balance
        let from_key = (token_address.to_vec(), from.to_vec());
        let from_balance = self.balances.get(&from_key).copied().unwrap_or(0);
        if from_balance < amount {
            return Err(TokenError::InsufficientBalance);
        }

        // Execute transfer
        self.balances.insert(from_key, from_balance - amount);

        let to_key = (token_address.to_vec(), to.to_vec());
        let to_balance = self.balances.get(&to_key).copied().unwrap_or(0);
        self.balances.insert(to_key, to_balance + amount);

        // Decrease allowance
        self.allowances.insert(allowance_key, allowance - amount);

        Ok(())
    }

    pub fn balance_of(&self, token_address: &[u8], owner: &[u8]) -> u64 {
        let key = (token_address.to_vec(), owner.to_vec());
        self.balances.get(&key).copied().unwrap_or(0)
    }

    #[allow(dead_code)]
    pub fn allowance(&self, token_address: &[u8], owner: &[u8], spender: &[u8]) -> u64 {
        let key = (token_address.to_vec(), owner.to_vec(), spender.to_vec());
        self.allowances.get(&key).copied().unwrap_or(0)
    }

    pub fn get_token(&self, address: &[u8]) -> Option<&CUR20Token> {
        self.tokens.get(address)
    }

    pub fn list_tokens(&self) -> Vec<&CUR20Token> {
        self.tokens.values().collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("token not found")]
    TokenNotFound,
    #[error("invalid token name (must be 1-64 characters)")]
    InvalidName,
    #[error("invalid token symbol (must be 1-12 characters)")]
    InvalidSymbol,
    #[error("invalid decimals (max 18)")]
    InvalidDecimals,
    #[error("invalid supply (must be positive)")]
    InvalidSupply,
    #[error("token already exists")]
    AlreadyExists,
    #[error("insufficient token balance")]
    InsufficientBalance,
    #[error("insufficient allowance")]
    InsufficientAllowance,
    #[error("cannot transfer to self")]
    SelfTransfer,
    #[error("amount must be positive")]
    ZeroAmount,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_deployer() -> Vec<u8> {
        vec![1u8; 20]
    }

    fn test_recipient() -> Vec<u8> {
        vec![2u8; 20]
    }

    fn test_spender() -> Vec<u8> {
        vec![3u8; 20]
    }

    fn deploy_test_token(registry: &mut TokenRegistry) -> Vec<u8> {
        let params = DeployTokenParams {
            name: "Test Token".to_string(),
            symbol: "TST".to_string(),
            decimals: 6,
            total_supply: 1_000_000_000,
        };
        registry
            .deploy_token(&test_deployer(), 0, &params, 1)
            .unwrap()
    }

    #[test]
    fn test_deploy_token() {
        let mut registry = TokenRegistry::new();
        let addr = deploy_test_token(&mut registry);
        assert_eq!(addr.len(), 20);
        let token = registry.get_token(&addr).unwrap();
        assert_eq!(token.name, "Test Token");
        assert_eq!(token.symbol, "TST");
        assert_eq!(token.decimals, 6);
        assert_eq!(token.total_supply, 1_000_000_000);
        assert_eq!(registry.balance_of(&addr, &test_deployer()), 1_000_000_000);
    }

    #[test]
    fn test_transfer_token() {
        let mut registry = TokenRegistry::new();
        let addr = deploy_test_token(&mut registry);
        registry
            .transfer(&addr, &test_deployer(), &test_recipient(), 500)
            .unwrap();
        assert_eq!(registry.balance_of(&addr, &test_deployer()), 999_999_500);
        assert_eq!(registry.balance_of(&addr, &test_recipient()), 500);
    }

    #[test]
    fn test_transfer_insufficient_balance() {
        let mut registry = TokenRegistry::new();
        let addr = deploy_test_token(&mut registry);
        let result = registry.transfer(&addr, &test_recipient(), &test_deployer(), 1);
        assert_eq!(result, Err(TokenError::InsufficientBalance));
    }

    #[test]
    fn test_approve_and_transfer_from() {
        let mut registry = TokenRegistry::new();
        let addr = deploy_test_token(&mut registry);
        registry
            .approve(&addr, &test_deployer(), &test_spender(), 1000)
            .unwrap();
        assert_eq!(
            registry.allowance(&addr, &test_deployer(), &test_spender()),
            1000
        );

        registry
            .transfer_from(
                &addr,
                &test_spender(),
                &test_deployer(),
                &test_recipient(),
                500,
            )
            .unwrap();
        assert_eq!(registry.balance_of(&addr, &test_deployer()), 999_999_500);
        assert_eq!(registry.balance_of(&addr, &test_recipient()), 500);
        assert_eq!(
            registry.allowance(&addr, &test_deployer(), &test_spender()),
            500
        );
    }

    #[test]
    fn test_transfer_from_insufficient_allowance() {
        let mut registry = TokenRegistry::new();
        let addr = deploy_test_token(&mut registry);
        registry
            .approve(&addr, &test_deployer(), &test_spender(), 100)
            .unwrap();
        let result = registry.transfer_from(
            &addr,
            &test_spender(),
            &test_deployer(),
            &test_recipient(),
            200,
        );
        assert_eq!(result, Err(TokenError::InsufficientAllowance));
    }

    #[test]
    fn test_deploy_duplicate() {
        let mut registry = TokenRegistry::new();
        deploy_test_token(&mut registry);
        // Different nonce -> different address, should succeed
        let params = DeployTokenParams {
            name: "Second Token".to_string(),
            symbol: "SEC".to_string(),
            decimals: 6,
            total_supply: 1_000_000,
        };
        let addr2 = registry
            .deploy_token(&test_deployer(), 1, &params, 1)
            .unwrap();
        assert!(registry.get_token(&addr2).is_some());
    }

    #[test]
    fn test_invalid_token_params() {
        let mut registry = TokenRegistry::new();
        let result = registry.deploy_token(
            &test_deployer(),
            0,
            &DeployTokenParams {
                name: "".to_string(),
                symbol: "TST".to_string(),
                decimals: 6,
                total_supply: 1000,
            },
            1,
        );
        assert_eq!(result, Err(TokenError::InvalidName));

        let result = registry.deploy_token(
            &test_deployer(),
            0,
            &DeployTokenParams {
                name: "Token".to_string(),
                symbol: "TST".to_string(),
                decimals: 19,
                total_supply: 1000,
            },
            1,
        );
        assert_eq!(result, Err(TokenError::InvalidDecimals));
    }

    #[test]
    fn test_transfer_zero_amount() {
        let mut registry = TokenRegistry::new();
        let addr = deploy_test_token(&mut registry);
        let result = registry.transfer(&addr, &test_deployer(), &test_recipient(), 0);
        assert_eq!(result, Err(TokenError::ZeroAmount));
    }

    #[test]
    fn test_self_transfer() {
        let mut registry = TokenRegistry::new();
        let addr = deploy_test_token(&mut registry);
        let result = registry.transfer(&addr, &test_deployer(), &test_deployer(), 100);
        assert_eq!(result, Err(TokenError::SelfTransfer));
    }

    #[test]
    fn test_list_tokens() {
        let mut registry = TokenRegistry::new();
        deploy_test_token(&mut registry);
        assert_eq!(registry.list_tokens().len(), 1);
    }
}
