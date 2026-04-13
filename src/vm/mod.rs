pub mod gas;
pub mod state;

use std::collections::HashMap;

use crate::core::receipt::{LogEntry, Receipt};
use crate::crypto::hash;
use state::ContractState;
use thiserror::Error;
use wasmer::{
    Function, FunctionEnv, FunctionEnvMut, Instance, Memory, MemoryAccessError, Module,
    RuntimeError, Store, Type, Value, imports,
    wasmparser::{Operator, Parser, Payload},
};

#[derive(Clone)]
struct VmExecutionContext {
    contract_id: Vec<u8>,
    storage: HashMap<Vec<u8>, Vec<u8>>,
    logs: Vec<LogEntry>,
    caller: Vec<u8>,
    input: i64,
    input_data: Vec<u8>,
    gas_remaining: u64,
    memory: Option<Memory>,
}

#[derive(Default)]
struct VmAnalysis {
    contains_loops: bool,
    has_explicit_fuel_hook: bool,
}

#[derive(Error, Debug)]
pub enum VmError {
    #[error("invalid wasm bytecode")]
    InvalidWasm,
    #[error("empty bytecode")]
    EmptyBytecode,
    #[error("gas limit exceeded: limit={limit}, needed={needed}")]
    OutOfGas { limit: u64, needed: u64 },
    #[error("missing contract entrypoint export `curs3d_call` or `call`")]
    MissingEntrypoint,
    #[error("unsupported contract entrypoint signature")]
    UnsupportedEntrypoint,
    #[error("contracts with loops must import `consume_gas` or `loop_tick`")]
    UnmeteredLoop,
    #[error("wasm execution failed: {0}")]
    Execution(String),
}

pub struct Vm;

impl Vm {
    fn bytes_to_i64(bytes: &[u8]) -> Option<i64> {
        if bytes.len() != 8 {
            return None;
        }
        let mut raw = [0u8; 8];
        raw.copy_from_slice(bytes);
        Some(i64::from_le_bytes(raw))
    }

    fn i64_to_bytes(value: i64) -> Vec<u8> {
        value.to_le_bytes().to_vec()
    }

    fn runtime_error(message: impl Into<String>) -> RuntimeError {
        RuntimeError::new(message.into())
    }

    fn memory_error(err: MemoryAccessError) -> RuntimeError {
        Self::runtime_error(format!("memory access failed: {}", err))
    }

    fn consume_gas(ctx: &mut VmExecutionContext, amount: u64) -> Result<(), VmError> {
        if ctx.gas_remaining < amount {
            return Err(VmError::OutOfGas {
                limit: ctx.gas_remaining,
                needed: amount,
            });
        }
        ctx.gas_remaining -= amount;
        Ok(())
    }

    fn charge_host_gas(
        env: &mut FunctionEnvMut<VmExecutionContext>,
        amount: u64,
    ) -> Result<(), RuntimeError> {
        Self::consume_gas(env.data_mut(), amount).map_err(|err| Self::runtime_error(err.to_string()))
    }

    fn memory_from_env(env: &FunctionEnvMut<VmExecutionContext>) -> Result<Memory, RuntimeError> {
        env.data()
            .memory
            .clone()
            .ok_or_else(|| Self::runtime_error("contract memory export `memory` is required"))
    }

    fn read_memory_bytes(
        env: &mut FunctionEnvMut<VmExecutionContext>,
        ptr: u32,
        len: u32,
    ) -> Result<Vec<u8>, RuntimeError> {
        let len = len as usize;
        let charge = gas::GAS_HOST_CALL_OVERHEAD
            .saturating_add((len as u64).saturating_mul(gas::GAS_MEMORY_READ_BYTE));
        Self::charge_host_gas(env, charge)?;
        let memory = Self::memory_from_env(env)?;
        let view = memory.view(&mut *env);
        let mut buffer = vec![0u8; len];
        view.read(ptr as u64, &mut buffer).map_err(Self::memory_error)?;
        Ok(buffer)
    }

    fn write_memory_bytes(
        env: &mut FunctionEnvMut<VmExecutionContext>,
        ptr: u32,
        data: &[u8],
    ) -> Result<(), RuntimeError> {
        let charge = gas::GAS_HOST_CALL_OVERHEAD
            .saturating_add((data.len() as u64).saturating_mul(gas::GAS_MEMORY_WRITE_BYTE));
        Self::charge_host_gas(env, charge)?;
        let memory = Self::memory_from_env(env)?;
        let view = memory.view(&mut *env);
        view.write(ptr as u64, data).map_err(Self::memory_error)
    }

    fn estimate_wasm_gas(code: &[u8]) -> Result<u64, VmError> {
        if code.starts_with(b"(module") {
            return Ok((code.len() as u64).saturating_mul(gas::GAS_WASM_DEFAULT_OP));
        }

        let mut total = 0u64;
        for payload in Parser::new(0).parse_all(code) {
            let payload = payload.map_err(|_| VmError::InvalidWasm)?;
            if let Payload::CodeSectionEntry(body) = payload {
                let mut reader = body.get_operators_reader().map_err(|_| VmError::InvalidWasm)?;
                while !reader.eof() {
                    let op = reader.read().map_err(|_| VmError::InvalidWasm)?;
                    total = total.saturating_add(Self::operator_gas_cost(&op));
                }
            }
        }
        Ok(total)
    }

    fn analyze_module(code: &[u8]) -> Result<VmAnalysis, VmError> {
        if code.starts_with(b"(module") {
            let source = std::str::from_utf8(code).map_err(|_| VmError::InvalidWasm)?;
            let contains_loops = source.contains("(loop") || source.contains(" loop");
            let has_explicit_fuel_hook =
                source.contains("\"consume_gas\"") || source.contains("\"loop_tick\"");
            return Ok(VmAnalysis {
                contains_loops,
                has_explicit_fuel_hook,
            });
        }

        let mut analysis = VmAnalysis::default();
        for payload in Parser::new(0).parse_all(code) {
            match payload.map_err(|_| VmError::InvalidWasm)? {
                Payload::ImportSection(reader) => {
                    for import in reader {
                        let import = import.map_err(|_| VmError::InvalidWasm)?;
                        if import.module == "curs3d"
                            && (import.name == "consume_gas" || import.name == "loop_tick")
                        {
                            analysis.has_explicit_fuel_hook = true;
                        }
                    }
                }
                Payload::CodeSectionEntry(body) => {
                    let mut reader =
                        body.get_operators_reader().map_err(|_| VmError::InvalidWasm)?;
                    while !reader.eof() {
                        let op = reader.read().map_err(|_| VmError::InvalidWasm)?;
                        if matches!(op, Operator::Loop { .. } | Operator::Br { .. } | Operator::BrIf { .. } | Operator::BrTable { .. }) {
                            analysis.contains_loops = true;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(analysis)
    }

    fn ensure_runtime_metering(code: &[u8]) -> Result<(), VmError> {
        let analysis = Self::analyze_module(code)?;
        if analysis.contains_loops && !analysis.has_explicit_fuel_hook {
            return Err(VmError::UnmeteredLoop);
        }
        Ok(())
    }

    fn operator_gas_cost(operator: &Operator<'_>) -> u64 {
        use Operator::*;

        match operator {
            Block { .. }
            | Loop { .. }
            | If { .. }
            | Else
            | End
            | Br { .. }
            | BrIf { .. }
            | BrTable { .. }
            | Return
            | Select
            | TypedSelect { .. } => gas::GAS_WASM_CONTROL_OP,
            Call { .. }
            | CallIndirect { .. }
            | ReturnCall { .. }
            | ReturnCallIndirect { .. } => gas::GAS_WASM_CALL_OP,
            I32Load { .. }
            | I64Load { .. }
            | F32Load { .. }
            | F64Load { .. }
            | I32Load8S { .. }
            | I32Load8U { .. }
            | I32Load16S { .. }
            | I32Load16U { .. }
            | I64Load8S { .. }
            | I64Load8U { .. }
            | I64Load16S { .. }
            | I64Load16U { .. }
            | I64Load32S { .. }
            | I64Load32U { .. }
            | I32Store { .. }
            | I64Store { .. }
            | F32Store { .. }
            | F64Store { .. }
            | I32Store8 { .. }
            | I32Store16 { .. }
            | I64Store8 { .. }
            | I64Store16 { .. }
            | I64Store32 { .. }
            | MemorySize { .. }
            | MemoryGrow { .. }
            | MemoryCopy { .. }
            | MemoryFill { .. }
            | MemoryInit { .. }
            | DataDrop { .. } => gas::GAS_WASM_MEMORY_OP,
            I32Const { .. }
            | I64Const { .. }
            | F32Const { .. }
            | F64Const { .. }
            | I32Eqz
            | I32Eq
            | I32Ne
            | I32LtS
            | I32LtU
            | I32GtS
            | I32GtU
            | I32LeS
            | I32LeU
            | I32GeS
            | I32GeU
            | I64Eqz
            | I64Eq
            | I64Ne
            | I64LtS
            | I64LtU
            | I64GtS
            | I64GtU
            | I64LeS
            | I64LeU
            | I64GeS
            | I64GeU
            | I32Add
            | I32Sub
            | I32Mul
            | I32DivS
            | I32DivU
            | I32RemS
            | I32RemU
            | I32And
            | I32Or
            | I32Xor
            | I32Shl
            | I32ShrS
            | I32ShrU
            | I64Add
            | I64Sub
            | I64Mul
            | I64DivS
            | I64DivU
            | I64RemS
            | I64RemU
            | I64And
            | I64Or
            | I64Xor
            | I64Shl
            | I64ShrS
            | I64ShrU => gas::GAS_WASM_NUMERIC_OP,
            _ => gas::GAS_WASM_DEFAULT_OP,
        }
    }

    fn build_imports(
        store: &mut Store,
        env: &FunctionEnv<VmExecutionContext>,
    ) -> wasmer::Imports {
        imports! {
            "curs3d" => {
                "storage_get" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>, key: i64| -> Result<i64, RuntimeError> {
                    let charge = gas::GAS_HOST_CALL_OVERHEAD.saturating_add(gas::GAS_STORAGE_READ);
                    Self::charge_host_gas(&mut env, charge)?;
                    Ok(env.data().storage.get(&Self::i64_to_bytes(key)).and_then(|value| Self::bytes_to_i64(value)).unwrap_or_default())
                }),
                "storage_set" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>, key: i64, value: i64| -> Result<(), RuntimeError> {
                    let charge = gas::GAS_HOST_CALL_OVERHEAD.saturating_add(gas::GAS_STORAGE_WRITE);
                    Self::charge_host_gas(&mut env, charge)?;
                    env.data_mut().storage.insert(Self::i64_to_bytes(key), Self::i64_to_bytes(value));
                    Ok(())
                }),
                "emit_log" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>, topic: i64, data: i64| -> Result<(), RuntimeError> {
                    let charge = gas::GAS_HOST_CALL_OVERHEAD.saturating_add(gas::GAS_LOG);
                    Self::charge_host_gas(&mut env, charge)?;
                    let ctx = env.data_mut();
                    ctx.logs.push(LogEntry {
                        contract: ctx.contract_id.clone(),
                        topics: vec![topic.to_le_bytes().to_vec()],
                        data: data.to_le_bytes().to_vec(),
                    });
                    Ok(())
                }),
                "input" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>| -> Result<i64, RuntimeError> {
                    let charge = gas::GAS_HOST_CALL_OVERHEAD.saturating_add(gas::GAS_PER_BYTE);
                    Self::charge_host_gas(&mut env, charge)?;
                    Ok(env.data().input)
                }),
                "consume_gas" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>, amount: i64| -> Result<(), RuntimeError> {
                    Self::charge_host_gas(&mut env, amount.max(0) as u64)?;
                    Ok(())
                }),
                "loop_tick" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>, amount: i64| -> Result<(), RuntimeError> {
                    Self::charge_host_gas(&mut env, amount.max(0) as u64)?;
                    Ok(())
                }),
                "storage_read" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>, key_ptr: i32, key_len: i32, dst_ptr: i32, dst_capacity: i32| -> Result<i32, RuntimeError> {
                    let key = Self::read_memory_bytes(&mut env, key_ptr.max(0) as u32, key_len.max(0) as u32)?;
                    let value = env.data().storage.get(&key).cloned().unwrap_or_default();
                    let copy_len = value.len().min(dst_capacity.max(0) as usize);
                    let charge = gas::GAS_STORAGE_READ
                        .saturating_add((value.len() as u64).saturating_mul(gas::GAS_MEMORY_WRITE_BYTE));
                    Self::charge_host_gas(&mut env, charge)?;
                    Self::write_memory_bytes(&mut env, dst_ptr.max(0) as u32, &value[..copy_len])?;
                    Ok(copy_len as i32)
                }),
                "storage_write_bytes" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>, key_ptr: i32, key_len: i32, value_ptr: i32, value_len: i32| -> Result<i32, RuntimeError> {
                    let key = Self::read_memory_bytes(&mut env, key_ptr.max(0) as u32, key_len.max(0) as u32)?;
                    let value = Self::read_memory_bytes(&mut env, value_ptr.max(0) as u32, value_len.max(0) as u32)?;
                    let charge = gas::GAS_STORAGE_WRITE
                        .saturating_add((value.len() as u64).saturating_mul(gas::GAS_PER_BYTE));
                    Self::charge_host_gas(&mut env, charge)?;
                    env.data_mut().storage.insert(key, value);
                    Ok(1)
                }),
                "emit_log_bytes" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>, topic_ptr: i32, topic_len: i32, data_ptr: i32, data_len: i32| -> Result<i32, RuntimeError> {
                    let topic = Self::read_memory_bytes(&mut env, topic_ptr.max(0) as u32, topic_len.max(0) as u32)?;
                    let data = Self::read_memory_bytes(&mut env, data_ptr.max(0) as u32, data_len.max(0) as u32)?;
                    let charge = gas::GAS_LOG
                        .saturating_add(((topic.len() + data.len()) as u64).saturating_mul(gas::GAS_PER_BYTE));
                    Self::charge_host_gas(&mut env, charge)?;
                    let ctx = env.data_mut();
                    ctx.logs.push(LogEntry {
                        contract: ctx.contract_id.clone(),
                        topics: vec![topic],
                        data,
                    });
                    Ok(1)
                }),
                "input_len" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>| -> Result<i32, RuntimeError> {
                    let charge = gas::GAS_HOST_CALL_OVERHEAD.saturating_add(gas::GAS_PER_BYTE);
                    Self::charge_host_gas(&mut env, charge)?;
                    Ok(env.data().input_data.len() as i32)
                }),
                "input_read" => Function::new_typed_with_env(store, env, |mut env: FunctionEnvMut<VmExecutionContext>, offset: i32, dst_ptr: i32, len: i32| -> Result<i32, RuntimeError> {
                    let offset = offset.max(0) as usize;
                    let len = len.max(0) as usize;
                    let data = env.data().input_data.clone();
                    if offset >= data.len() {
                        return Ok(0);
                    }
                    let end = offset.saturating_add(len).min(data.len());
                    let slice = &data[offset..end];
                    Self::write_memory_bytes(&mut env, dst_ptr.max(0) as u32, slice)?;
                    Ok(slice.len() as i32)
                }),
            }
        }
    }

    fn compile_module(code: &[u8]) -> Result<Module, VmError> {
        let store = Store::default();
        Module::new(&store, code).map_err(|_| VmError::InvalidWasm)
    }

    pub fn deploy(
        code: &[u8],
        deployer: &[u8],
        nonce: u64,
        gas_limit: u64,
    ) -> Result<(ContractState, Receipt), VmError> {
        if code.is_empty() {
            return Err(VmError::EmptyBytecode);
        }

        Self::ensure_runtime_metering(code)?;
        let _module = Self::compile_module(code)?;

        let gas_needed = gas::GAS_BASE_TX
            .saturating_add(gas::GAS_DEPLOY)
            .saturating_add((code.len() as u64).saturating_mul(gas::GAS_PER_BYTE));

        if gas_limit < gas_needed {
            return Err(VmError::OutOfGas {
                limit: gas_limit,
                needed: gas_needed,
            });
        }

        let mut addr_input = deployer.to_vec();
        addr_input.extend_from_slice(&nonce.to_le_bytes());
        let contract_address = hash::sha3_hash(&addr_input)[..hash::ADDRESS_LEN].to_vec();
        let code_hash = hash::sha3_hash(code);

        let contract = ContractState {
            code_hash: code_hash.clone(),
            code: code.to_vec(),
            storage: HashMap::new(),
            owner: deployer.to_vec(),
        };

        let receipt = Receipt {
            tx_hash: Vec::new(),
            success: true,
            gas_used: gas_needed,
            logs: Vec::new(),
            return_data: Vec::new(),
            contract_address: Some(contract_address),
        };

        Ok((contract, receipt))
    }

    pub fn call(
        contract: &mut ContractState,
        function_data: &[u8],
        caller: &[u8],
        _value: u64,
        gas_limit: u64,
    ) -> Result<Receipt, VmError> {
        Self::ensure_runtime_metering(&contract.code)?;
        let static_execution_gas = Self::estimate_wasm_gas(&contract.code)?;
        let gas_needed = gas::GAS_BASE_TX
            .saturating_add(gas::GAS_CALL)
            .saturating_add((function_data.len() as u64).saturating_mul(gas::GAS_PER_BYTE))
            .saturating_add(static_execution_gas);

        if gas_limit < gas_needed {
            return Err(VmError::OutOfGas {
                limit: gas_limit,
                needed: gas_needed,
            });
        }

        let mut store = Store::default();
        let module = Module::new(&store, &contract.code).map_err(|_| VmError::InvalidWasm)?;
        let input = function_data
            .get(..8)
            .and_then(Self::bytes_to_i64)
            .unwrap_or_default();
        let env = FunctionEnv::new(
            &mut store,
            VmExecutionContext {
                contract_id: contract.code_hash[..hash::ADDRESS_LEN.min(contract.code_hash.len())]
                    .to_vec(),
                storage: contract.storage.clone(),
                logs: Vec::new(),
                caller: caller.to_vec(),
                input,
                input_data: function_data.to_vec(),
                gas_remaining: gas_limit.saturating_sub(gas_needed),
                memory: None,
            },
        );
        let imports = Self::build_imports(&mut store, &env);
        let instance = Instance::new(&mut store, &module, &imports)
            .map_err(|err| VmError::Execution(err.to_string()))?;
        let memory = instance.exports.get_memory("memory").ok().cloned();
        env.as_mut(&mut store).memory = memory;

        let function = instance
            .exports
            .get_function("curs3d_call")
            .or_else(|_| instance.exports.get_function("call"))
            .map_err(|_| VmError::MissingEntrypoint)?;
        let results = match function.ty(&store).params() {
            [] => function.call(&mut store, &[]),
            [Type::I32] => function.call(&mut store, &[Value::I32(input as i32)]),
            [Type::I64] => function.call(&mut store, &[Value::I64(input)]),
            _ => return Err(VmError::UnsupportedEntrypoint),
        }
        .map_err(|err| VmError::Execution(err.to_string()))?;

        let mut return_data = Vec::new();
        for value in results {
            match value {
                Value::I32(value) => return_data.extend_from_slice(&value.to_le_bytes()),
                Value::I64(value) => return_data.extend_from_slice(&value.to_le_bytes()),
                Value::F32(value) => return_data.extend_from_slice(&value.to_bits().to_le_bytes()),
                Value::F64(value) => return_data.extend_from_slice(&value.to_bits().to_le_bytes()),
                Value::V128(value) => return_data.extend_from_slice(&value.to_le_bytes()),
                _ => {}
            }
        }

        let ctx = env.as_ref(&store);
        contract.storage = ctx.storage.clone();

        let receipt = Receipt {
            tx_hash: Vec::new(),
            success: true,
            gas_used: gas_limit.saturating_sub(ctx.gas_remaining),
            logs: if ctx.logs.is_empty() {
                vec![LogEntry {
                    contract: ctx.contract_id.clone(),
                    topics: vec![hash::sha3_hash(b"curs3d_call")],
                    data: ctx
                        .caller
                        .iter()
                        .cloned()
                        .chain(function_data.iter().cloned())
                        .collect(),
                }]
            } else {
                ctx.logs.clone()
            },
            return_data,
            contract_address: None,
        };

        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_wasm() -> Vec<u8> {
        br#"(module
            (memory (export "memory") 1)
            (func (export "curs3d_call") (result i32)
                i32.const 7)
        )"#
        .to_vec()
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
        assert_eq!(receipt.return_data, 7i32.to_le_bytes().to_vec());
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
    fn test_call_storage_and_logs() {
        let deployer = vec![1u8; 20];
        let module = br#"(module
            (import "curs3d" "storage_write_bytes" (func $storage_write_bytes (param i32 i32 i32 i32) (result i32)))
            (import "curs3d" "storage_read" (func $storage_read (param i32 i32 i32 i32) (result i32)))
            (import "curs3d" "emit_log_bytes" (func $emit_log_bytes (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "key")
            (data (i32.const 16) "value")
            (data (i32.const 32) "topic")
            (func (export "curs3d_call") (result i32)
                i32.const 0
                i32.const 3
                i32.const 16
                i32.const 5
                call $storage_write_bytes
                drop
                i32.const 0
                i32.const 3
                i32.const 64
                i32.const 16
                call $storage_read
                drop
                i32.const 32
                i32.const 5
                i32.const 64
                i32.const 5
                call $emit_log_bytes
                drop
                i32.const 5)
        )"#;
        let (mut contract, _) = Vm::deploy(module, &deployer, 0, 1_000_000).unwrap();
        let caller = vec![2u8; 20];
        let receipt = Vm::call(&mut contract, b"ignored", &caller, 0, 1_000_000).unwrap();
        assert_eq!(receipt.return_data, 5i32.to_le_bytes().to_vec());
        assert_eq!(
            contract.storage.get(b"key".as_slice()).cloned().unwrap(),
            b"value".to_vec()
        );
        assert_eq!(receipt.logs.len(), 1);
        assert_eq!(receipt.logs[0].topics[0], b"topic".to_vec());
        assert_eq!(receipt.logs[0].data, b"value".to_vec());
    }

    #[test]
    fn test_deterministic_contract_address() {
        let deployer = vec![1u8; 20];
        let (_, receipt1) = Vm::deploy(&valid_wasm(), &deployer, 0, 1_000_000).unwrap();
        let (_, receipt2) = Vm::deploy(&valid_wasm(), &deployer, 0, 1_000_000).unwrap();
        assert_eq!(receipt1.contract_address, receipt2.contract_address);

        let (_, receipt3) = Vm::deploy(&valid_wasm(), &deployer, 1, 1_000_000).unwrap();
        assert_ne!(receipt1.contract_address, receipt3.contract_address);
    }

    #[test]
    fn test_rejects_unmetered_loop_contract() {
        let deployer = vec![1u8; 20];
        let loop_contract = br#"(module
            (memory (export "memory") 1)
            (func (export "curs3d_call")
                (loop
                    br 0))
        )"#;
        let err = Vm::deploy(loop_contract, &deployer, 0, 1_000_000).unwrap_err();
        assert!(matches!(err, VmError::UnmeteredLoop));
    }
}
