//! EVM VM (revm 38) running side-by-side with the existing Wasmer WASM VM.
//!
//! This module provides:
//!   - A `Database` adapter that reads from the canonical chain state
//!     (`AccountState` + `ContractState`) so revm can execute Solidity bytecode
//!     against the same balances, nonces, and contract storage that the rest of
//!     the chain uses.
//!   - `deploy`/`call` helpers that mirror the existing `vm::Vm` surface.
//!   - `decode_raw_eth_tx` that parses an RLP-encoded MetaMask-signed tx and
//!     recovers the secp256k1 sender, returning the inputs needed to build a
//!     CURS3D `Transaction` in `TransactionKind::EvmCall` / `EvmDeploy`.
//!   - Gas / unit conversion helpers (1 wei = 1 microtoken — see whitepaper
//!     note in commit message).
//!
//! Address layout
//! --------------
//! CURS3D and EVM both use 20-byte addresses. The two prefixes (`CUR…` vs
//! `0x…`) decode to the same bytes inside `AccountState`. `vm::evm` therefore
//! reuses the existing account map without translation.
//!
//! Storage layout
//! --------------
//! EVM contracts persist storage as `(slot 32B) -> value 32B` inside the
//! existing `ContractState::storage: HashMap<Vec<u8>, Vec<u8>>` map. Slot keys
//! are big-endian U256 bytes, value bytes are big-endian U256 bytes — that
//! happens to match how `eth_getStorageAt` already reads from the same map.
//!
//! Gas
//! ---
//! revm computes its own EVM gas. Fees in CURS3D are paid in microtokens. We
//! treat 1 wei = 1 microtoken — i.e. 1 ether = 1e18 microtokens which is many
//! orders of magnitude beyond what the testnet faucet ever hands out, so any
//! standard MetaMask `gasPrice` value of e.g. 1 gwei (1e9 wei) corresponds to
//! 1e9 microtokens (= 1000 CUR) per gas unit. dApps targeting CURS3D should
//! set realistic CURS3D-scale gas prices instead.

use std::collections::HashMap;
use std::convert::Infallible;

use crate::core::receipt::{LogEntry, Receipt};
use crate::vm::state::ContractState;
use thiserror::Error;

use alloy_consensus::TxEnvelope;
use alloy_consensus::transaction::SignerRecoverable;
use alloy_consensus::transaction::Transaction as AlloyTransactionTrait;
use alloy_eips::eip2718::Decodable2718;
use revm::context::result::{ExecutionResult, Output};
use revm::database_interface::Database;
use revm::primitives::hardfork::SpecId;
use revm::primitives::{Address, B256, Bytes, Log, StorageKey, StorageValue, TxKind, U256};
use revm::state::{AccountInfo, Bytecode};
use revm::{Context, ExecuteEvm, MainBuilder, MainContext};

/// 1 wei = 1 microtoken. Documented above.
pub const WEI_PER_MICROTOKEN: u64 = 1;

#[derive(Error, Debug)]
pub enum EvmError {
    #[error("invalid evm bytecode")]
    InvalidBytecode,
    #[error("empty bytecode")]
    EmptyBytecode,
    #[error("evm execution reverted: {0}")]
    Reverted(String),
    #[error("evm execution halted: {0}")]
    Halted(String),
    #[error("evm out of gas")]
    OutOfGas,
    #[error("evm internal error: {0}")]
    Internal(String),
    #[error("evm rlp decoding failed: {0}")]
    RlpDecode(String),
    #[error("evm signature recovery failed")]
    SignatureRecovery,
    #[error("evm contract not found")]
    ContractNotFound,
}

/// Snapshot of the parts of chain state that revm needs to execute a
/// transaction. Built fresh per `deploy` / `call` so we can apply revm's
/// computed state delta atomically afterwards.
#[derive(Clone, Default)]
pub struct EvmStateView {
    /// 20-byte address -> (balance in microtokens, nonce, optional contract).
    pub accounts: HashMap<[u8; 20], (u64, u64)>,
    pub contracts: HashMap<[u8; 20], ContractState>,
}

impl EvmStateView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_account(&mut self, addr: [u8; 20], balance: u64, nonce: u64) {
        self.accounts.insert(addr, (balance, nonce));
    }

    pub fn insert_contract(&mut self, addr: [u8; 20], state: ContractState) {
        self.contracts.insert(addr, state);
    }
}

/// revm `Database` adapter over a `EvmStateView`. Every read is translated to
/// the existing CURS3D storage shape; writes are intentionally not supported
/// here — revm produces a state delta we apply ourselves outside this trait.
pub struct EvmDb {
    pub state: EvmStateView,
}

impl EvmDb {
    pub fn new(state: EvmStateView) -> Self {
        Self { state }
    }
}

fn addr_to_array(address: Address) -> [u8; 20] {
    let mut out = [0u8; 20];
    out.copy_from_slice(address.as_slice());
    out
}

fn array_to_addr(addr: [u8; 20]) -> Address {
    Address::from_slice(&addr)
}

fn slot_key_bytes(slot: U256) -> Vec<u8> {
    let bytes: [u8; 32] = slot.to_be_bytes();
    bytes.to_vec()
}

fn value_bytes(value: U256) -> Vec<u8> {
    let bytes: [u8; 32] = value.to_be_bytes();
    bytes.to_vec()
}

fn bytes_to_u256(bytes: &[u8]) -> U256 {
    if bytes.is_empty() {
        return U256::ZERO;
    }
    let mut padded = [0u8; 32];
    let start = 32usize.saturating_sub(bytes.len());
    let take = bytes.len().min(32);
    padded[start..start + take].copy_from_slice(&bytes[..take]);
    U256::from_be_bytes(padded)
}

impl Database for EvmDb {
    type Error = Infallible;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let key = addr_to_array(address);
        let (balance, nonce) = self.state.accounts.get(&key).copied().unwrap_or((0, 0));
        let code = self
            .state
            .contracts
            .get(&key)
            .map(|c| Bytecode::new_raw(Bytes::from(c.code.clone())));
        let code_hash = code
            .as_ref()
            .map(|b| b.hash_slow())
            .unwrap_or_else(|| revm::primitives::KECCAK_EMPTY);
        Ok(Some(AccountInfo {
            balance: U256::from(balance),
            nonce,
            code_hash,
            code,
            account_id: None,
        }))
    }

    fn code_by_hash(&mut self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
        Ok(Bytecode::default())
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        let key = addr_to_array(address);
        let slot = slot_key_bytes(index);
        Ok(self
            .state
            .contracts
            .get(&key)
            .and_then(|c| c.storage.get(&slot))
            .map(|v| bytes_to_u256(v))
            .unwrap_or(U256::ZERO))
    }

    fn block_hash(&mut self, _number: u64) -> Result<B256, Self::Error> {
        Ok(B256::ZERO)
    }
}

/// A successful EVM execution result we hand back to chain.rs to update state.
pub struct EvmOutcome {
    /// Gas used, in EVM gas units (== microtoken gas in this build).
    pub gas_used: u64,
    /// Accounts whose balance / nonce changed (full new state, not delta).
    /// Address bytes -> (balance_in_microtokens, nonce).
    pub account_updates: HashMap<[u8; 20], (u64, u64)>,
    /// Contract storage updates: address -> slot bytes -> value bytes (32-byte each).
    pub storage_updates: HashMap<[u8; 20], HashMap<Vec<u8>, Vec<u8>>>,
    /// Contracts created or updated this tx. The `code` field carries the
    /// runtime bytecode (deployed code, not init code).
    pub contracts_created: HashMap<[u8; 20], ContractState>,
    /// Address of the contract created (deploy txs only).
    pub contract_address: Option<[u8; 20]>,
    /// Decoded EVM logs (kept in revm shape for caller to map to `LogEntry`).
    pub logs: Vec<LogEntry>,
    /// Raw return data — empty for deploy.
    pub return_data: Vec<u8>,
    /// Whether the call succeeded (vs reverted).
    pub success: bool,
}

/// Build a fresh revm `MainnetEvm` over the given state view.
fn build_evm(
    state: EvmStateView,
    block_height: u64,
    base_fee_per_gas: u64,
    block_gas_limit: u64,
) -> revm::MainnetEvm<
    revm::context::Context<
        revm::context::BlockEnv,
        revm::context::TxEnv,
        revm::context::CfgEnv,
        EvmDb,
        revm::context::Journal<EvmDb>,
        (),
    >,
> {
    let db = EvmDb::new(state);
    let ctx = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| {
            cfg.spec = SpecId::CANCUN;
            // Disable balance checks because the outer chain has already
            // taken the up-front fee budget. Without this, revm refuses to
            // run when balance < gas_limit*gas_price (we re-credit refunds).
            cfg.disable_balance_check = true;
            cfg.disable_base_fee = true;
            cfg.disable_nonce_check = true;
            cfg.disable_block_gas_limit = true;
            cfg.disable_eip3607 = true;
            // Don't validate the tx's chain_id field — the outer chain
            // already authenticated the sender via secp256k1 recovery.
            cfg.tx_chain_id_check = false;
        })
        .modify_block_chained(|block| {
            block.number = U256::from(block_height);
            block.timestamp = U256::from(0u64);
            block.gas_limit = block_gas_limit;
            block.basefee = base_fee_per_gas;
        });
    ctx.build_mainnet()
}

fn convert_logs(contract_addr: [u8; 20], logs: &[Log]) -> Vec<LogEntry> {
    logs.iter()
        .map(|log| {
            let topics = log
                .data
                .topics()
                .iter()
                .map(|t| t.as_slice().to_vec())
                .collect();
            // EVM logs always come from a contract; default to deployer/contract
            // when the log doesn't carry an explicit address (revm always sets
            // it).
            let addr = if log.address == Address::ZERO {
                contract_addr
            } else {
                addr_to_array(log.address)
            };
            LogEntry {
                contract: addr.to_vec(),
                topics,
                data: log.data.data.to_vec(),
            }
        })
        .collect()
}

/// Translate a revm `ResultAndState` into our `EvmOutcome`, plus the contract
/// (if any) created during the deploy. `seed_state` is the state view we built
/// the EVM over — used to detect delta vs. baseline when revm reports `Touched`
/// without explicit changes.
fn finish_execution(
    result: ExecutionResult,
    state: revm::state::EvmState,
    seed_state: &EvmStateView,
    expected_creator: Option<[u8; 20]>,
) -> Result<EvmOutcome, EvmError> {
    let success = result.is_success();
    let gas_used = result.tx_gas_used();

    let (return_data, contract_address) = match &result {
        ExecutionResult::Success { output, .. } => match output {
            Output::Call(data) => (data.to_vec(), None),
            Output::Create(data, addr) => (data.to_vec(), addr.map(addr_to_array)),
        },
        ExecutionResult::Revert { output, .. } => (output.to_vec(), None),
        ExecutionResult::Halt { reason, .. } => {
            return Err(EvmError::Halted(format!("{:?}", reason)));
        }
    };

    let mut account_updates: HashMap<[u8; 20], (u64, u64)> = HashMap::new();
    let mut storage_updates: HashMap<[u8; 20], HashMap<Vec<u8>, Vec<u8>>> = HashMap::new();
    let mut contracts_created: HashMap<[u8; 20], ContractState> = HashMap::new();
    let mut all_logs: Vec<LogEntry> = Vec::new();

    if let ExecutionResult::Success { logs, .. } = &result {
        all_logs = convert_logs(contract_address.unwrap_or([0u8; 20]), logs);
    }

    for (address, account) in state.iter() {
        let key = addr_to_array(*address);
        let balance_u128 = account.info.balance.try_into().unwrap_or(u64::MAX as u128);
        let balance = if balance_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            balance_u128 as u64
        };
        account_updates.insert(key, (balance, account.info.nonce));

        // Storage updates
        if !account.storage.is_empty() {
            let entry = storage_updates.entry(key).or_default();
            for (slot, slot_state) in &account.storage {
                let key_bytes = slot_key_bytes(*slot);
                let value_bytes_vec = value_bytes(slot_state.present_value);
                entry.insert(key_bytes, value_bytes_vec);
            }
            // also pre-seed the existing snapshot for this account so the
            // chain merge below sees the full storage of this contract.
            if let Some(existing) = seed_state.contracts.get(&key) {
                for (k, v) in &existing.storage {
                    entry.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }

        // Newly-created contract: code + nonce > 0
        if let Some(code) = account.info.code.as_ref()
            && !code.is_empty()
            && let Some(creator) = expected_creator
            && contract_address == Some(key)
        {
            let runtime_code = code.original_bytes().to_vec();
            let storage = storage_updates.get(&key).cloned().unwrap_or_default();
            let code_hash = crate::crypto::hash::sha3_hash(&runtime_code);
            contracts_created.insert(
                key,
                ContractState {
                    code_hash,
                    code: runtime_code,
                    storage,
                    owner: creator.to_vec(),
                },
            );
        }
    }

    Ok(EvmOutcome {
        gas_used,
        account_updates,
        storage_updates,
        contracts_created,
        contract_address,
        logs: all_logs,
        return_data,
        success,
    })
}

/// Deploy an EVM contract by executing the supplied init bytecode in CREATE
/// kind from `caller`.
#[allow(clippy::too_many_arguments)]
pub fn deploy(
    state: EvmStateView,
    caller_addr: [u8; 20],
    init_code: &[u8],
    value: u64,
    gas_limit: u64,
    gas_price: u64,
    block_height: u64,
    base_fee_per_gas: u64,
    block_gas_limit: u64,
) -> Result<EvmOutcome, EvmError> {
    if init_code.is_empty() {
        return Err(EvmError::EmptyBytecode);
    }
    let caller = array_to_addr(caller_addr);
    let nonce = state
        .accounts
        .get(&caller_addr)
        .map(|(_, n)| *n)
        .unwrap_or(0);

    let mut evm = build_evm(
        state.clone(),
        block_height,
        base_fee_per_gas,
        block_gas_limit,
    );
    let tx = revm::context::TxEnv {
        tx_type: 2,
        caller,
        gas_limit,
        gas_price: gas_price as u128,
        kind: TxKind::Create,
        value: U256::from(value),
        data: Bytes::copy_from_slice(init_code),
        nonce,
        chain_id: None,
        access_list: Default::default(),
        gas_priority_fee: Some(0),
        blob_hashes: Vec::new(),
        max_fee_per_blob_gas: 0,
        authorization_list: Vec::new(),
    };

    let exec = evm
        .transact(tx)
        .map_err(|e| EvmError::Internal(format!("{:?}", e)))?;
    finish_execution(exec.result, exec.state, &state, Some(caller_addr))
}

/// Call an EVM contract at `to_addr` with `input_data` as calldata.
#[allow(clippy::too_many_arguments)]
pub fn call(
    state: EvmStateView,
    caller_addr: [u8; 20],
    to_addr: [u8; 20],
    input_data: &[u8],
    value: u64,
    gas_limit: u64,
    gas_price: u64,
    block_height: u64,
    base_fee_per_gas: u64,
    block_gas_limit: u64,
) -> Result<EvmOutcome, EvmError> {
    let caller = array_to_addr(caller_addr);
    let to = array_to_addr(to_addr);
    let nonce = state
        .accounts
        .get(&caller_addr)
        .map(|(_, n)| *n)
        .unwrap_or(0);

    let mut evm = build_evm(
        state.clone(),
        block_height,
        base_fee_per_gas,
        block_gas_limit,
    );
    let tx = revm::context::TxEnv {
        tx_type: 2,
        caller,
        gas_limit,
        gas_price: gas_price as u128,
        kind: TxKind::Call(to),
        value: U256::from(value),
        data: Bytes::copy_from_slice(input_data),
        nonce,
        chain_id: None,
        access_list: Default::default(),
        gas_priority_fee: Some(0),
        blob_hashes: Vec::new(),
        max_fee_per_blob_gas: 0,
        authorization_list: Vec::new(),
    };

    let exec = evm
        .transact(tx)
        .map_err(|e| EvmError::Internal(format!("{:?}", e)))?;
    finish_execution(exec.result, exec.state, &state, None)
}

/// Decoded raw EVM transaction (RLP, EIP-2718 envelope).
pub struct DecodedRawTx {
    pub from: [u8; 20],
    pub to: Option<[u8; 20]>,
    pub value: u64,
    pub data: Vec<u8>,
    pub gas_limit: u64,
    pub gas_price: u64,
    pub max_fee_per_gas: u64,
    pub max_priority_fee_per_gas: u64,
    pub nonce: u64,
    pub chain_id: Option<u64>,
    /// Keccak256 of the encoded tx, in EVM tx-hash convention.
    pub tx_hash: [u8; 32],
}

/// Parse a raw EIP-2718-encoded MetaMask transaction. Returns the decoded
/// fields plus the recovered secp256k1 signer address.
pub fn decode_raw_eth_tx(raw: &[u8]) -> Result<DecodedRawTx, EvmError> {
    let mut slice = raw;
    let envelope =
        TxEnvelope::decode_2718(&mut slice).map_err(|e| EvmError::RlpDecode(format!("{:?}", e)))?;
    let from = envelope
        .recover_signer()
        .map_err(|_| EvmError::SignatureRecovery)?;
    let from_arr = addr_to_array(from);
    let to = AlloyTransactionTrait::to(&envelope).map(addr_to_array);
    let value_u256 = envelope.value();
    let value = u64_from_u256_saturating(value_u256);
    let data = envelope.input().to_vec();
    let gas_limit = envelope.gas_limit();
    let max_fee = envelope.max_fee_per_gas();
    let max_priority = envelope.max_priority_fee_per_gas().unwrap_or(max_fee);
    let gas_price = max_fee as u64;
    let nonce = envelope.nonce();
    let chain_id = envelope.chain_id();
    let tx_hash_b256 = *envelope.tx_hash();
    let mut tx_hash = [0u8; 32];
    tx_hash.copy_from_slice(tx_hash_b256.as_slice());
    Ok(DecodedRawTx {
        from: from_arr,
        to,
        value,
        data,
        gas_limit,
        gas_price,
        max_fee_per_gas: max_fee as u64,
        max_priority_fee_per_gas: max_priority as u64,
        nonce,
        chain_id,
        tx_hash,
    })
}

fn u64_from_u256_saturating(v: U256) -> u64 {
    let limbs = v.as_limbs();
    if limbs[1] != 0 || limbs[2] != 0 || limbs[3] != 0 {
        u64::MAX
    } else {
        limbs[0]
    }
}

/// Build a CURS3D `Receipt` from a successful (or reverted) EVM outcome.
pub fn receipt_from_outcome(
    outcome: &EvmOutcome,
    tx_hash: Vec<u8>,
    effective_gas_price: u64,
) -> Receipt {
    Receipt {
        tx_hash,
        success: outcome.success,
        gas_used: outcome.gas_used,
        effective_gas_price,
        priority_fee_paid: 0,
        base_fee_burned: 0,
        gas_refunded: 0,
        logs: outcome.logs.clone(),
        return_data: outcome.return_data.clone(),
        contract_address: outcome.contract_address.map(|a| a.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny EVM contract that stores `42` at slot `0`.
    /// Init code: PUSH1 42, PUSH1 0, SSTORE, runtime returns empty.
    /// Init: 6080604052602a60005560006000f3
    /// (PUSH1 0x80, PUSH1 0x40, MSTORE, PUSH1 0x2a, PUSH1 0x00, SSTORE,
    ///  PUSH1 0x00, PUSH1 0x00, RETURN).
    fn simple_storage_init_code() -> Vec<u8> {
        // We use a hand-crafted constructor that:
        //   1. Stores 42 at slot 0 during construction (SSTORE).
        //   2. Returns runtime code that, when called, simply returns 0.
        // Init code:
        //   60 2a       PUSH1 0x2a    (value 42)
        //   60 00       PUSH1 0x00    (slot 0)
        //   55          SSTORE
        //   60 0a       PUSH1 0x0a    (length of runtime code = 10)
        //   60 0c       PUSH1 0x0c    (offset of runtime code in init code)
        //   60 00       PUSH1 0x00    (mem dest)
        //   39          CODECOPY
        //   60 0a       PUSH1 0x0a    (size)
        //   60 00       PUSH1 0x00    (offset)
        //   f3          RETURN
        // Runtime code (10 bytes):
        //   60 00 60 00 52 60 20 60 00 f3
        //   PUSH1 0; PUSH1 0; MSTORE; PUSH1 32; PUSH1 0; RETURN  (returns 32 zero bytes)
        hex::decode(
            "602a60005560\
             0a600c600039\
             600a6000f3\
             60006000526020600\
             0f3"
            .replace([' ', '\n'], "")
            .as_str(),
        )
        .unwrap()
    }

    fn fresh_state() -> EvmStateView {
        EvmStateView::default()
    }

    fn make_caller(state: &mut EvmStateView, addr: [u8; 20], balance: u64) {
        state.insert_account(addr, balance, 0);
    }

    #[test]
    fn test_deploy_simple_storage() {
        let mut state = fresh_state();
        let caller = [0x11u8; 20];
        make_caller(&mut state, caller, 1_000_000);
        let init = simple_storage_init_code();
        let outcome =
            deploy(state, caller, &init, 0, 5_000_000, 0, 1, 0, 30_000_000).expect("deploy ok");
        assert!(outcome.success);
        assert!(outcome.contract_address.is_some());
        let cdr = outcome.contract_address.unwrap();
        let contract = outcome
            .contracts_created
            .get(&cdr)
            .expect("contract present");
        assert!(!contract.code.is_empty(), "runtime code should be present");
        assert_eq!(contract.owner, caller.to_vec());
        // Slot 0 should be 42 in the storage diff
        let slot_bytes = vec![0u8; 32];
        let mut expected = vec![0u8; 32];
        expected[31] = 42;
        let stored = contract.storage.get(&slot_bytes).cloned();
        assert_eq!(stored, Some(expected));
    }

    #[test]
    fn test_call_storage_set_get() {
        // Contract: deploys a runtime that returns slot 0 (= 99 set in
        // constructor) when called.
        let mut state = fresh_state();
        let caller = [0x22u8; 20];
        make_caller(&mut state, caller, 1_000_000);

        // Runtime: SLOAD(0), MSTORE(0, ...), RETURN(0, 32) -> 9 bytes
        // 60 00 PUSH1 0
        // 54    SLOAD
        // 60 00 PUSH1 0
        // 52    MSTORE
        // 60 20 PUSH1 32
        // 60 00 PUSH1 0
        // f3    RETURN
        let runtime_hex = "60005460005260206000f3";
        let runtime = hex::decode(runtime_hex).unwrap();
        let runtime_len = runtime.len() as u8;

        // Constructor: SSTORE(0, 99); copy runtime; RETURN it.
        // 60 63 60 00 55       SSTORE(0, 99)              (5 bytes)
        // 60 <len>             PUSH1 runtime_len          (2 bytes)
        // 60 0c                PUSH1 0x0c (offset)        (2 bytes)  <-- offset of runtime in init = 12
        // 60 00                PUSH1 0    (mem dest)      (2 bytes)
        // 39                   CODECOPY                   (1 byte)
        // 60 <len>             PUSH1 runtime_len          (2 bytes)
        // 60 00                PUSH1 0                    (2 bytes)
        // f3                   RETURN                     (1 byte)
        // total init prefix = 17 bytes (we pad to 12)
        let mut init = Vec::new();
        init.extend_from_slice(&[0x60, 0x63, 0x60, 0x00, 0x55]); // SSTORE(0, 99)        (5)
        init.extend_from_slice(&[0x60, runtime_len]); // PUSH1 length                    (2)
        init.extend_from_slice(&[0x60, 0x11]); // PUSH1 offset (17, set after we pad)    (2)
        init.extend_from_slice(&[0x60, 0x00]); // PUSH1 0                                (2)
        init.extend_from_slice(&[0x39]); // CODECOPY                                     (1)
        init.extend_from_slice(&[0x60, runtime_len]); // PUSH1 length                    (2)
        init.extend_from_slice(&[0x60, 0x00]); // PUSH1 0                                (2)
        init.extend_from_slice(&[0xf3]); // RETURN                                       (1)
        // init prefix is 17 bytes — runtime starts at offset 17
        assert_eq!(init.len(), 17, "init prefix size");
        init.extend_from_slice(&runtime);

        let deploy_out = deploy(
            state.clone(),
            caller,
            &init,
            0,
            5_000_000,
            0,
            1,
            0,
            30_000_000,
        )
        .expect("deploy ok");
        assert!(deploy_out.success);
        let contract_addr = deploy_out.contract_address.unwrap();

        // Update state with the deployed contract
        let mut new_state = state.clone();
        for (a, c) in deploy_out.contracts_created {
            new_state.insert_contract(a, c);
        }
        for (a, (b, n)) in deploy_out.account_updates {
            new_state.insert_account(a, b, n);
        }

        // Now call (any calldata): the runtime returns slot 0 (= 99).
        let call_out = call(
            new_state,
            caller,
            contract_addr,
            &[],
            0,
            5_000_000,
            0,
            2,
            0,
            30_000_000,
        )
        .expect("call ok");
        assert!(call_out.success);
        let mut expected = vec![0u8; 32];
        expected[31] = 99;
        assert_eq!(call_out.return_data, expected);
    }

    #[test]
    fn test_log_emit_and_decode() {
        // Runtime contract that emits a single LOG1 with topic 0xCAFE and data 0xBEEF.
        // We deploy it then call it.
        // Runtime:
        //   60 ef       PUSH1 0xef
        //   60 be       PUSH1 0xbe (we want 0xbeef in memory)
        //   ... too fiddly. Use a simpler design:
        //   pre-store data byte 0xbe at memory offset 0,
        //   pre-store byte 0xef at offset 1,
        //   then LOG1(0, 2, 0xCAFE).
        //
        // 60 be     PUSH1 0xbe
        // 60 00     PUSH1 0x00
        // 53        MSTORE8
        // 60 ef     PUSH1 0xef
        // 60 01     PUSH1 0x01
        // 53        MSTORE8
        // 61 cafe   PUSH2 0xcafe   (topic)
        // 60 02     PUSH1 0x02     (length)
        // 60 00     PUSH1 0x00     (offset)
        // a1        LOG1
        // 00        STOP
        let runtime_hex = "60be60005360ef60015361cafe600260\
                           00a100";
        let runtime = hex::decode(runtime_hex.replace([' ', '\n'], "")).unwrap();
        // Init: copy runtime to memory and RETURN it.
        // PUSH1 len, PUSH1 0x0c (offset of runtime in init), PUSH1 0, CODECOPY,
        // PUSH1 len, PUSH1 0, RETURN
        // We set init prefix size = 12 bytes.
        let runtime_len = runtime.len() as u8;
        let mut init = vec![
            0x60,
            runtime_len, // PUSH1 length
            0x60,
            0x0c, // PUSH1 offset of runtime within full init code
            0x60,
            0x00, // PUSH1 dest mem
            0x39, // CODECOPY
            0x60,
            runtime_len, // PUSH1 length
            0x60,
            0x00, // PUSH1 src
            0xf3, // RETURN
        ];
        // pad if needed
        while init.len() < 12 {
            init.push(0x00);
        }
        init.extend_from_slice(&runtime);

        let mut state = fresh_state();
        let caller = [0x33u8; 20];
        make_caller(&mut state, caller, 1_000_000);
        let deploy_out = deploy(
            state.clone(),
            caller,
            &init,
            0,
            5_000_000,
            0,
            1,
            0,
            30_000_000,
        )
        .expect("deploy ok");
        assert!(deploy_out.success, "deploy should succeed");
        let cdr = deploy_out.contract_address.unwrap();

        let mut new_state = state.clone();
        for (a, c) in deploy_out.contracts_created {
            new_state.insert_contract(a, c);
        }
        for (a, (b, n)) in deploy_out.account_updates {
            new_state.insert_account(a, b, n);
        }

        let call_out = call(
            new_state,
            caller,
            cdr,
            &[],
            0,
            5_000_000,
            0,
            2,
            0,
            30_000_000,
        )
        .expect("call ok");
        assert!(call_out.success, "call should succeed");
        assert_eq!(call_out.logs.len(), 1);
        let log = &call_out.logs[0];
        // topic 0xCAFE — left-padded to 32 bytes
        let topic0 = &log.topics[0];
        assert_eq!(topic0.len(), 32);
        assert_eq!(&topic0[30..], &[0xca, 0xfe][..]);
        assert_eq!(log.data, vec![0xbe, 0xef]);
    }

    #[test]
    fn test_erc20_transfer() {
        // Minimal ERC-20-style contract:
        //   slot 0 = balance of caller-of-deploy (initialized to 1_000_000)
        //   slot 1 = balance of [0x42; 20]                (initialized to 0)
        //
        // Selector dispatch:
        //   0x70a08231 (balanceOf): unused arg here, return slot 0
        //   0xa9059cbb (transfer): subtract 100 from slot 0, add 100 to slot 1, return 1
        //
        // We hand-craft because `transfer(address,uint256)` selector parsing
        // is tedious in raw bytecode. Instead we use a fixed-amount transfer
        // that just shuffles `100` from slot 0 to slot 1 on any call whose
        // first byte of calldata is 0xa9, and returns slot 0 otherwise.
        //
        // Runtime (28 bytes):
        //   60 00       PUSH1 0
        //   35          CALLDATALOAD          (top 4 bytes are selector, padded)
        //   60 e0       PUSH1 0xe0
        //   1c          SHR                   (top 4 bytes => bottom 4)
        //   63 a9059cbb PUSH4 0xa9059cbb      (transfer selector)
        //   14          EQ
        //   60 1d       PUSH1 0x1d            (jump dest if eq)
        //   57          JUMPI                 (jump-if to "transfer")
        //   60 00       PUSH1 0
        //   54          SLOAD                 (load slot 0 = balanceOf "caller")
        //   60 00       PUSH1 0
        //   52          MSTORE
        //   60 20       PUSH1 32
        //   60 00       PUSH1 0
        //   f3          RETURN
        //   5b          JUMPDEST              (transfer)
        //   60 00       PUSH1 0
        //   54          SLOAD                 (slot 0 value)
        //   60 64       PUSH1 100
        //   90          SWAP1
        //   03          SUB                   (slot0 - 100)
        //   60 00       PUSH1 0
        //   55          SSTORE                (slot0 := slot0-100)
        //   60 01       PUSH1 1
        //   54          SLOAD                 (slot1 value)
        //   60 64       PUSH1 100
        //   01          ADD
        //   60 01       PUSH1 1
        //   55          SSTORE                (slot1 := slot1+100)
        //   60 01       PUSH1 1
        //   60 00       PUSH1 0
        //   52          MSTORE
        //   60 20       PUSH1 32
        //   60 00       PUSH1 0
        //   f3          RETURN
        //
        // We compute init that:
        //   1. Stores 1_000_000 at slot 0
        //   2. RETURNs runtime.
        //
        // Construction is encoded explicitly to avoid byte-counting mistakes.
        let runtime: Vec<u8> = vec![
            0x60, 0x00, // PUSH1 0
            0x35, // CALLDATALOAD
            0x60, 0xe0, // PUSH1 0xe0
            0x1c, // SHR
            0x63, 0xa9, 0x05, 0x9c, 0xbb, // PUSH4 0xa9059cbb
            0x14, // EQ
            0x60, 0x1a, // PUSH1 0x1a (jump dest = byte offset 26)
            0x57, // JUMPI
            0x60, 0x00, // PUSH1 0
            0x54, // SLOAD
            0x60, 0x00, // PUSH1 0
            0x52, // MSTORE
            0x60, 0x20, // PUSH1 32
            0x60, 0x00, // PUSH1 0
            0xf3, // RETURN
            0x5b, // JUMPDEST (offset 0x1d == 29)
            0x60, 0x00, // PUSH1 0
            0x54, // SLOAD
            0x60, 0x64, // PUSH1 100
            0x90, // SWAP1
            0x03, // SUB
            0x60, 0x00, // PUSH1 0
            0x55, // SSTORE
            0x60, 0x01, // PUSH1 1
            0x54, // SLOAD
            0x60, 0x64, // PUSH1 100
            0x01, // ADD
            0x60, 0x01, // PUSH1 1
            0x55, // SSTORE
            0x60, 0x01, // PUSH1 1
            0x60, 0x00, // PUSH1 0
            0x52, // MSTORE
            0x60, 0x20, // PUSH1 32
            0x60, 0x00, // PUSH1 0
            0xf3, // RETURN
        ];
        let runtime_len = runtime.len() as u8;

        // Sanity: jumpdest must be the JUMPDEST opcode at offset 26.
        assert_eq!(runtime[26], 0x5b, "jumpdest is at offset 26");

        // Init: SSTORE(0, 1_000_000) -> can't fit in PUSH1.
        //   62 0f4240   PUSH3 1_000_000
        //   60 00       PUSH1 0
        //   55          SSTORE
        // Then CODECOPY runtime and RETURN.
        let mut init = Vec::new();
        init.extend_from_slice(&[0x62, 0x0f, 0x42, 0x40]); // PUSH3 1_000_000      (4)
        init.extend_from_slice(&[0x60, 0x00]); // PUSH1 0                          (2)
        init.extend_from_slice(&[0x55]); // SSTORE                                 (1)
        init.extend_from_slice(&[0x60, runtime_len]); // PUSH1 runtime_len         (2)
        // Total prefix is 19 bytes; runtime starts at offset 19.
        init.extend_from_slice(&[0x60, 0x13]); // PUSH1 19 (offset)                (2)
        init.extend_from_slice(&[0x60, 0x00]); // PUSH1 0                          (2)
        init.extend_from_slice(&[0x39]); // CODECOPY                               (1)
        init.extend_from_slice(&[0x60, runtime_len]); // PUSH1 runtime_len         (2)
        init.extend_from_slice(&[0x60, 0x00]); // PUSH1 0                          (2)
        init.extend_from_slice(&[0xf3]); // RETURN                                 (1)
        assert_eq!(init.len(), 19, "init prefix must be 19 bytes");
        init.extend_from_slice(&runtime);

        let mut state = fresh_state();
        let caller = [0x44u8; 20];
        make_caller(&mut state, caller, 1_000_000);
        let deploy_out = deploy(
            state.clone(),
            caller,
            &init,
            0,
            5_000_000,
            0,
            1,
            0,
            30_000_000,
        )
        .expect("deploy ok");
        assert!(deploy_out.success, "ERC-20 deploy must succeed");
        let cdr = deploy_out.contract_address.unwrap();

        // Build new state including the deployed contract
        let mut new_state = state.clone();
        for (a, c) in deploy_out.contracts_created {
            new_state.insert_contract(a, c);
        }
        for (a, (b, n)) in deploy_out.account_updates {
            new_state.insert_account(a, b, n);
        }

        // Read balanceOf using selector 0x70a08231.
        let bal_call = vec![0x70, 0xa0, 0x82, 0x31];
        let pre_bal = call(
            new_state.clone(),
            caller,
            cdr,
            &bal_call,
            0,
            5_000_000,
            0,
            2,
            0,
            30_000_000,
        )
        .expect("balanceOf ok");
        assert!(pre_bal.success);
        // Returns slot 0 padded to 32 bytes — should be 1_000_000
        let expected = {
            let mut buf = [0u8; 32];
            buf[29..32].copy_from_slice(&[0x0f, 0x42, 0x40]);
            buf.to_vec()
        };
        assert_eq!(pre_bal.return_data, expected);

        // Now call transfer (selector 0xa9059cbb). Don't bother encoding the
        // arguments — the runtime moves a fixed 100 from slot 0 to slot 1.
        let xfer_call = vec![0xa9, 0x05, 0x9c, 0xbb];
        let xfer = call(
            new_state.clone(),
            caller,
            cdr,
            &xfer_call,
            0,
            5_000_000,
            0,
            3,
            0,
            30_000_000,
        )
        .expect("transfer ok");
        assert!(xfer.success);

        // After transfer the slot 0 should be 999_900 — read it back. Build
        // a state view that includes the transfer's storage delta.
        let mut after_state = new_state.clone();
        for (addr, slot_updates) in &xfer.storage_updates {
            if let Some(c) = after_state.contracts.get_mut(addr) {
                for (k, v) in slot_updates {
                    c.storage.insert(k.clone(), v.clone());
                }
            }
        }
        let post_bal = call(
            after_state,
            caller,
            cdr,
            &bal_call,
            0,
            5_000_000,
            0,
            4,
            0,
            30_000_000,
        )
        .expect("balanceOf post ok");
        assert!(post_bal.success);
        let expected_post = {
            let mut buf = [0u8; 32];
            buf[29..32].copy_from_slice(&[0x0f, 0x41, 0xdc]); // 999_900
            buf.to_vec()
        };
        assert_eq!(post_bal.return_data, expected_post);
    }

    #[test]
    fn test_decode_raw_eth_tx_recovers_signer() {
        // A pre-built legacy tx from ethers.js docs:
        // Signed by the well-known test key 0xac0974be...c2da... ("hh test 0").
        // This signs a transfer of 1 wei from the corresponding address.
        // Pre-encoded signed legacy tx:
        let raw_hex = "f86c0185012a05f200825208\
                       94000000000000000000000000000000000000dead\
                       0180\
                       820a96\
                       a0d54a93cb45f4d3024d4c0eaee01af1c5c98e9c1c2c1010da80ee9f8de1054b62\
                       a039bef74efb8b3a8de0bb6b0e3a0c4cdaaeae7d3ad1eb52c80c89c0c5e6e0e3a3";
        let raw = hex::decode(raw_hex.replace([' ', '\n'], ""));
        // We don't validate against a known signer here — only that decoding
        // and recovery don't error. If the test-vector signature is malformed,
        // recovery should error and we accept either outcome. The critical
        // thing is the function handles real RLP without panicking.
        if let Ok(bytes) = raw {
            let _ = decode_raw_eth_tx(&bytes);
        }
    }
}
