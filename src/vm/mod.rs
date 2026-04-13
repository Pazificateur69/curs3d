pub mod gas;
pub mod state;

use crate::core::receipt::{LogEntry, Receipt};
use crate::crypto::hash;
use state::ContractState;
use thiserror::Error;

/// WASM magic bytes: \0asm
const WASM_MAGIC: &[u8] = b"\0asm";

#[derive(Error, Debug)]
pub enum VmError {
    #[error("invalid wasm bytecode: missing magic bytes")]
    InvalidWasm,
    #[error("empty bytecode")]
    EmptyBytecode,
    #[error("gas limit exceeded: limit={limit}, needed={needed}")]
    OutOfGas { limit: u64, needed: u64 },
    #[error("contract not found")]
    ContractNotFound,
}

pub struct Vm;

impl Vm {
    /// Deploy a new contract. Validates WASM magic bytes, computes the contract
    /// address deterministically from deployer + nonce, and returns the new
    /// ContractState together with a Receipt.
    pub fn deploy(
        code: &[u8],
        deployer: &[u8],
        nonce: u64,
        gas_limit: u64,
    ) -> Result<(ContractState, Receipt), VmError> {
        // Validate bytecode is not empty
        if code.is_empty() {
            return Err(VmError::EmptyBytecode);
        }

        // Validate WASM magic bytes
        if code.len() < 4 || &code[..4] != WASM_MAGIC {
            return Err(VmError::InvalidWasm);
        }

        // Calculate gas: base tx + deploy cost + per-byte cost for code
        let gas_needed = gas::GAS_BASE_TX
            .saturating_add(gas::GAS_DEPLOY)
            .saturating_add((code.len() as u64).saturating_mul(gas::GAS_PER_BYTE));

        if gas_limit < gas_needed {
            return Err(VmError::OutOfGas {
                limit: gas_limit,
                needed: gas_needed,
            });
        }

        // Derive contract address deterministically: sha3(deployer || nonce)[..20]
        let mut addr_input = deployer.to_vec();
        addr_input.extend_from_slice(&nonce.to_le_bytes());
        let contract_address = hash::sha3_hash(&addr_input)[..hash::ADDRESS_LEN].to_vec();

        let code_hash = hash::sha3_hash(code);

        let contract = ContractState {
            code_hash: code_hash.clone(),
            code: code.to_vec(),
            storage: std::collections::HashMap::new(),
            owner: deployer.to_vec(),
        };

        let receipt = Receipt {
            tx_hash: Vec::new(), // filled in by caller
            success: true,
            gas_used: gas_needed,
            logs: Vec::new(),
            return_data: Vec::new(),
            contract_address: Some(contract_address),
        };

        Ok((contract, receipt))
    }

    /// Call an existing contract. Currently a stub that validates gas and returns
    /// a successful receipt. Full WASM execution will be added later.
    pub fn call(
        contract: &mut ContractState,
        function_data: &[u8],
        caller: &[u8],
        _value: u64,
        gas_limit: u64,
    ) -> Result<Receipt, VmError> {
        // Calculate gas: base tx + call cost + per-byte cost for input data
        let gas_needed = gas::GAS_BASE_TX
            .saturating_add(gas::GAS_CALL)
            .saturating_add((function_data.len() as u64).saturating_mul(gas::GAS_PER_BYTE));

        if gas_limit < gas_needed {
            return Err(VmError::OutOfGas {
                limit: gas_limit,
                needed: gas_needed,
            });
        }

        // Stub: write a log entry recording the call for observability
        let log = LogEntry {
            contract: hash::sha3_hash(&contract.code_hash)[..hash::ADDRESS_LEN].to_vec(),
            topics: vec![hash::sha3_hash(b"call")],
            data: caller.to_vec(),
        };

        let receipt = Receipt {
            tx_hash: Vec::new(), // filled in by caller
            success: true,
            gas_used: gas_needed,
            logs: vec![log],
            return_data: Vec::new(),
            contract_address: None,
        };

        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_wasm() -> Vec<u8> {
        // Minimal valid WASM header
        b"\0asm\x01\x00\x00\x00".to_vec()
    }

    #[test]
    fn test_deploy_valid_wasm() {
        let deployer = vec![1u8; 20];
        let (contract, receipt) = Vm::deploy(&valid_wasm(), &deployer, 0, 1_000_000).unwrap();
        assert!(receipt.success);
        assert!(receipt.contract_address.is_some());
        assert_eq!(receipt.contract_address.as_ref().unwrap().len(), 20);
        assert!(!contract.code.is_empty());
        assert_eq!(contract.owner, deployer);
        assert!(receipt.gas_used > 0);
    }

    #[test]
    fn test_deploy_invalid_wasm() {
        let deployer = vec![1u8; 20];
        let result = Vm::deploy(b"not-wasm", &deployer, 0, 1_000_000);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VmError::InvalidWasm));
    }

    #[test]
    fn test_deploy_empty_bytecode() {
        let deployer = vec![1u8; 20];
        let result = Vm::deploy(b"", &deployer, 0, 1_000_000);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VmError::EmptyBytecode));
    }

    #[test]
    fn test_deploy_out_of_gas() {
        let deployer = vec![1u8; 20];
        let result = Vm::deploy(&valid_wasm(), &deployer, 0, 100);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VmError::OutOfGas { .. }));
    }

    #[test]
    fn test_call_returns_receipt() {
        let deployer = vec![1u8; 20];
        let (mut contract, _) = Vm::deploy(&valid_wasm(), &deployer, 0, 1_000_000).unwrap();
        let caller = vec![2u8; 20];
        let receipt = Vm::call(&mut contract, b"do_something", &caller, 0, 1_000_000).unwrap();
        assert!(receipt.success);
        assert!(receipt.gas_used > 0);
        assert_eq!(receipt.logs.len(), 1);
    }

    #[test]
    fn test_call_out_of_gas() {
        let deployer = vec![1u8; 20];
        let (mut contract, _) = Vm::deploy(&valid_wasm(), &deployer, 0, 1_000_000).unwrap();
        let caller = vec![2u8; 20];
        let result = Vm::call(&mut contract, b"do_something", &caller, 0, 100);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VmError::OutOfGas { .. }));
    }

    #[test]
    fn test_deterministic_contract_address() {
        let deployer = vec![1u8; 20];
        let (_, receipt1) = Vm::deploy(&valid_wasm(), &deployer, 0, 1_000_000).unwrap();
        let (_, receipt2) = Vm::deploy(&valid_wasm(), &deployer, 0, 1_000_000).unwrap();
        assert_eq!(receipt1.contract_address, receipt2.contract_address);

        // Different nonce -> different address
        let (_, receipt3) = Vm::deploy(&valid_wasm(), &deployer, 1, 1_000_000).unwrap();
        assert_ne!(receipt1.contract_address, receipt3.contract_address);
    }
}
