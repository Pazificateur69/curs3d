//! Ethereum-compatible JSON-RPC subset.
//!
//! Exposes an Ethereum-shaped subset of `eth_*` / `net_*` / `web3_*` methods so
//! standard EVM tooling (MetaMask, ethers.js, wagmi, hardhat, foundry) can read
//! the CURS3D testnet and submit ECDSA-signed RLP transactions through revm.
//! Native CURS3D transactions still use the ML-DSA path at `/api/tx/submit`.
//!
//! Wire format: standard JSON-RPC 2.0. Both single requests and batches accepted.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::core::block::Block;
use crate::core::chain::Blockchain;
use crate::core::receipt::{IndexedLogEntry, IndexedReceipt, LogFilter};
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::hash;

const CLIENT_VERSION: &str = concat!("curs3d/v", env!("CARGO_PKG_VERSION"), "/rust");

/// Maximum block range allowed for a single `eth_getLogs` call. Picked to
/// match common production RPC providers (e.g. Alchemy, Infura). A wide-open
/// range against an indexed-but-large chain forces the node to scan every
/// block in the window and return an unbounded array, which is a cheap DoS
/// vector. (#9)
const MAX_LOG_RANGE: u64 = 10_000;

/// Stable numeric chain id derived from the chain_id string (first 4 bytes of SHA-3,
/// masked into a 31-bit positive integer to fit safely in a JS Number / EIP-155).
pub fn numeric_chain_id(chain_id: &str) -> u64 {
    let digest = hash::sha3_hash(chain_id.as_bytes());
    let bytes = [digest[0], digest[1], digest[2], digest[3]];
    let raw = u32::from_be_bytes(bytes);
    (raw & 0x7FFFFFFF) as u64
}

fn hex_u64(value: u64) -> String {
    format!("0x{:x}", value)
}

fn hex_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        "0x".to_string()
    } else {
        format!("0x{}", hex::encode(bytes))
    }
}

fn hex_storage_word(bytes: &[u8]) -> String {
    let mut word = [0u8; 32];
    if bytes.len() >= 32 {
        word.copy_from_slice(&bytes[bytes.len() - 32..]);
    } else {
        word[32 - bytes.len()..].copy_from_slice(bytes);
    }
    format!("0x{}", hex::encode(word))
}

fn parse_hex_address(value: &str) -> Option<Vec<u8>> {
    let value = value.trim();
    let stripped = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("CUR"))
        .unwrap_or(value);
    let bytes = hex::decode(stripped).ok()?;
    if bytes.len() == hash::ADDRESS_LEN {
        Some(bytes)
    } else {
        None
    }
}

fn parse_hex_hash(value: &str) -> Option<Vec<u8>> {
    let value = value.trim();
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(stripped).ok()
}

fn parse_hex_quantity(value: &str) -> Option<u64> {
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    if stripped.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(stripped, 16).ok()
}

/// Ethereum-style 32-byte storage slot. Accepts a `0x`-prefixed (or bare) hex
/// string; left-pads to 32 bytes if shorter, rejects anything longer than 32.
/// This matches what `eth_getStorageAt` clients on the Ethereum mainnet expect.
fn parse_storage_slot(value: &str) -> Option<Vec<u8>> {
    let stripped = value.trim().strip_prefix("0x").unwrap_or(value.trim());
    if stripped.is_empty() {
        return Some(vec![0u8; 32]);
    }
    let mut padded = stripped.to_string();
    if padded.len() % 2 == 1 {
        padded.insert(0, '0');
    }
    let bytes = hex::decode(&padded).ok()?;
    if bytes.len() > 32 {
        return None;
    }
    let mut out = vec![0u8; 32];
    out[32 - bytes.len()..].copy_from_slice(&bytes);
    Some(out)
}

/// Resolve a block tag ("latest", "earliest", "pending", "finalized", "safe", or hex height).
fn resolve_block_tag(chain: &Blockchain, tag: &Value) -> Option<u64> {
    match tag {
        Value::String(s) => match s.as_str() {
            "latest" | "pending" => Some(chain.height()),
            "earliest" => Some(0),
            "finalized" | "safe" => Some(chain.finality_tracker.finalized_height),
            other => parse_hex_quantity(other),
        },
        Value::Null => Some(chain.height()),
        _ => None,
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn rpc_success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// The wire-level transaction hash exposed to Ethereum tooling.
///
/// EVM transactions submitted through `eth_sendRawTransaction` are referenced
/// by clients (MetaMask, forge, ethers, viem) using `keccak256(rlp_signed)` —
/// the hash they computed locally before broadcasting. CURS3D internally
/// indexes by `Transaction::hash()` (a SHA3 over our `bincode` shape).
/// Returning the internal hash to EVM clients makes `eth_getTransactionByHash`
/// and `eth_getTransactionReceipt` answer `null` for hashes the client itself
/// produced, which trips Foundry's broadcast finalization.
///
/// For native Dilithium-signed transactions, no client-side keccak hash
/// exists, so we fall back to the internal hash.
fn eth_wire_hash(tx: &Transaction) -> String {
    if tx.is_evm()
        && let Ok(decoded) = crate::vm::evm::decode_raw_eth_tx(&tx.evm_raw_tx)
    {
        return format!("0x{}", hex::encode(decoded.tx_hash));
    }
    format!("0x{}", tx.hash_hex())
}

/// Resolve an Ethereum-style hash from a client to a chain location, checking
/// both the EVM-hash index and the internal-hash index (the same hash bytes
/// can land in either depending on whether the tx was native or EVM).
fn resolve_eth_tx_lookup(chain: &Blockchain, hash: &[u8]) -> Option<(u64, usize)> {
    if let Some(loc) = chain.evm_tx_hash_index.get(hash).copied() {
        return Some(loc);
    }
    chain.tx_hash_index.get(hash).copied()
}

fn block_to_eth(chain: &Blockchain, block: &Block, full_tx: bool) -> Value {
    let txs = if full_tx {
        let mut entries: Vec<Value> = Vec::with_capacity(block.transactions.len());
        for (idx, tx) in block.transactions.iter().enumerate() {
            entries.push(tx_to_eth(chain, tx, Some((block.header.height, idx))));
        }
        Value::Array(entries)
    } else {
        Value::Array(
            block
                .transactions
                .iter()
                .map(|tx| Value::String(eth_wire_hash(tx)))
                .collect(),
        )
    };

    // sha3Uncles is the empty-uncle-list keccak256 — Ethereum tooling (foundry,
    // ethers, web3.js) requires this field even on chains without uncle blocks.
    // Constant: keccak256(rlp([])) = 0x1dcc4de8...
    const EMPTY_UNCLE_HASH: &str =
        "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347";
    // mixHash is a PoW remnant; many libraries deserialize it. Set to zero hash.
    const ZERO_HASH: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

    json!({
        "number": hex_u64(block.header.height),
        "hash": hex_bytes(&block.hash),
        "parentHash": hex_bytes(&block.header.prev_hash),
        "sha3Uncles": EMPTY_UNCLE_HASH,
        "mixHash": ZERO_HASH,
        // `.max(0)` clamps any pre-1970 timestamp to 0 (genesis edge case).
        // After the clamp the value is non-negative so `cast_unsigned` is
        // lossless; using it makes clippy::cast_sign_loss happy.
        "timestamp": hex_u64(block.header.timestamp.max(0).cast_unsigned()),
        "miner": hex_bytes(&hash::address_bytes_from_public_key(&block.header.validator_public_key)),
        "validator": hex_bytes(&block.header.validator_public_key),
        "stateRoot": hex_bytes(&block.header.state_root),
        "transactionsRoot": hex_bytes(&block.header.merkle_root),
        "receiptsRoot": hex_bytes(&block.header.merkle_root),
        "logsBloom": "0x".to_string() + &"0".repeat(512),
        "difficulty": "0x0",
        "totalDifficulty": "0x0",
        "size": hex_u64(block.transactions.len() as u64),
        "gasLimit": hex_u64(chain.genesis_config.block_gas_limit),
        "gasUsed": "0x0",
        "extraData": "0x",
        "nonce": "0x0000000000000000",
        "uncles": [],
        "baseFeePerGas": hex_u64(chain.next_base_fee_per_gas(block)),
        "transactions": txs,
    })
}

fn tx_to_eth(chain: &Blockchain, tx: &Transaction, location: Option<(u64, usize)>) -> Value {
    let (block_number, block_hash, tx_index) = match location {
        Some((h, i)) => (
            Value::String(hex_u64(h)),
            chain
                .blocks
                .get(h as usize)
                .map(|b| Value::String(hex_bytes(&b.hash)))
                .unwrap_or(Value::Null),
            Value::String(hex_u64(i as u64)),
        ),
        None => (Value::Null, Value::Null, Value::Null),
    };
    let to = if tx.kind == TransactionKind::DeployEvmContract {
        Value::Null
    } else {
        Value::String(hex_bytes(&tx.to))
    };
    json!({
        "hash": eth_wire_hash(tx),
        "from": hex_bytes(&tx.from),
        "to": to,
        "value": hex_u64(tx.amount),
        "nonce": hex_u64(tx.nonce),
        "gas": hex_u64(tx.gas_limit),
        "gasPrice": hex_u64(tx.max_fee_per_gas.max(tx.fee)),
        "maxFeePerGas": hex_u64(tx.max_fee_per_gas),
        "maxPriorityFeePerGas": hex_u64(tx.max_priority_fee_per_gas),
        "input": hex_bytes(&tx.data),
        "blockNumber": block_number,
        "blockHash": block_hash,
        "transactionIndex": tx_index,
        "type": "0x2",
        "chainId": hex_u64(numeric_chain_id(&tx.chain_id)),
    })
}

fn receipt_to_eth(chain: &Blockchain, indexed: &IndexedReceipt) -> Value {
    let block = chain.blocks.get(indexed.block_height as usize);
    let block_hash = block
        .map(|b| hex_bytes(&b.hash))
        .unwrap_or_else(|| "0x".into());
    // Return the Ethereum-shape hash if the underlying tx is EVM, so forge /
    // ethers can match it against what they got from `eth_sendRawTransaction`.
    let tx_hash = block
        .and_then(|b| b.transactions.get(indexed.tx_index))
        .map(eth_wire_hash)
        .unwrap_or_else(|| format!("0x{}", hex::encode(&indexed.tx_hash)));
    let tx = block.and_then(|b| b.transactions.get(indexed.tx_index));
    let from = tx
        .map(|tx| Value::String(hex_bytes(&tx.from)))
        .unwrap_or(Value::Null);
    let to = tx
        .map(|tx| {
            if tx.kind == TransactionKind::DeployEvmContract {
                Value::Null
            } else {
                Value::String(hex_bytes(&tx.to))
            }
        })
        .unwrap_or(Value::Null);

    let logs: Vec<Value> = indexed
        .receipt
        .logs
        .iter()
        .enumerate()
        .map(|(log_index, log)| {
            json!({
                "address": hex_bytes(&log.contract),
                "topics": log.topics.iter().map(|t| hex_bytes(t)).collect::<Vec<_>>(),
                "data": hex_bytes(&log.data),
                "blockNumber": hex_u64(indexed.block_height),
                "blockHash": block_hash,
                "transactionHash": tx_hash,
                "transactionIndex": hex_u64(indexed.tx_index as u64),
                "logIndex": hex_u64(log_index as u64),
                "removed": false,
            })
        })
        .collect();

    json!({
        "transactionHash": tx_hash,
        "transactionIndex": hex_u64(indexed.tx_index as u64),
        "blockHash": block_hash,
        "blockNumber": hex_u64(indexed.block_height),
        "from": from,
        "to": to,
        "cumulativeGasUsed": hex_u64(indexed.receipt.gas_used),
        "gasUsed": hex_u64(indexed.receipt.gas_used),
        "effectiveGasPrice": hex_u64(indexed.receipt.effective_gas_price),
        "contractAddress": indexed
            .receipt
            .contract_address
            .as_ref()
            .map(|c| Value::String(hex_bytes(c)))
            .unwrap_or(Value::Null),
        "logs": logs,
        "logsBloom": "0x".to_string() + &"0".repeat(512),
        "status": if indexed.receipt.success { "0x1" } else { "0x0" },
        "type": "0x2",
    })
}

fn log_to_eth(chain: &Blockchain, log: &IndexedLogEntry) -> Value {
    let block = chain.blocks.get(log.block_height as usize);
    let block_hash = block
        .map(|b| hex_bytes(&b.hash))
        .unwrap_or_else(|| "0x".into());
    // Map the internal tx_hash to the Ethereum wire hash when the source tx
    // is EVM, so wallets/explorers see consistent hashes across receipt /
    // tx / log.
    let tx_hash = block
        .and_then(|b| b.transactions.get(log.tx_index))
        .map(eth_wire_hash)
        .unwrap_or_else(|| hex_bytes(&log.tx_hash));
    json!({
        "address": hex_bytes(&log.contract),
        "topics": log.topics.iter().map(|t| hex_bytes(t)).collect::<Vec<_>>(),
        "data": hex_bytes(&log.data),
        "blockNumber": hex_u64(log.block_height),
        "blockHash": block_hash,
        "transactionHash": tx_hash,
        "transactionIndex": hex_u64(log.tx_index as u64),
        "logIndex": hex_u64(log.log_index as u64),
        "removed": false,
    })
}

fn estimate_eth_gas(params: &[Value]) -> u64 {
    let call_obj = params.first().and_then(|v| v.as_object());
    let Some(call_obj) = call_obj else {
        return 21_000;
    };

    let is_deploy = call_obj
        .get("to")
        .and_then(|v| v.as_str())
        .and_then(parse_hex_address)
        .is_none();
    let data_len = call_obj
        .get("data")
        .or_else(|| call_obj.get("input"))
        .and_then(|v| v.as_str())
        .and_then(parse_hex_hash)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0);

    let intrinsic = 21_000u64.saturating_add(data_len.saturating_mul(16));
    let estimated = if is_deploy {
        intrinsic.saturating_add(5_000_000).max(8_000_000)
    } else if data_len == 0 {
        intrinsic
    } else {
        intrinsic.saturating_add(500_000).max(750_000)
    };

    estimated.min(crate::core::chain::DEFAULT_BLOCK_GAS_LIMIT)
}

/// Process a single JSON-RPC request and return the response value.
async fn dispatch(chain: &Arc<Mutex<Blockchain>>, request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = match request.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => return rpc_error(id, -32600, "Invalid Request: missing method"),
    };
    let params = request
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]));
    let params_arr = params.as_array().cloned().unwrap_or_default();

    match method {
        "web3_clientVersion" => rpc_success(id, json!(CLIENT_VERSION)),
        "web3_sha3" => {
            let Some(input) = params_arr.first().and_then(|v| v.as_str()) else {
                return rpc_error(id, -32602, "expected hex string parameter");
            };
            let Some(bytes) = parse_hex_hash(input) else {
                return rpc_error(id, -32602, "invalid hex input");
            };
            rpc_success(id, json!(hex_bytes(&hash::sha3_hash(&bytes))))
        }
        "net_version" => {
            let chain = chain.lock().await;
            rpc_success(id, json!(numeric_chain_id(chain.chain_id()).to_string()))
        }
        "net_listening" => rpc_success(id, json!(true)),
        "net_peerCount" => rpc_success(id, json!("0x0")),
        "eth_chainId" => {
            let chain = chain.lock().await;
            rpc_success(id, json!(hex_u64(numeric_chain_id(chain.chain_id()))))
        }
        "eth_protocolVersion" => rpc_success(id, json!("0x41")),
        "eth_syncing" => rpc_success(id, json!(false)),
        "eth_mining" => rpc_success(id, json!(false)),
        "eth_hashrate" => rpc_success(id, json!("0x0")),
        "eth_accounts" => rpc_success(id, json!([])),
        "eth_blockNumber" => {
            let chain = chain.lock().await;
            rpc_success(id, json!(hex_u64(chain.height())))
        }
        "eth_gasPrice" => {
            let chain = chain.lock().await;
            let base = chain.next_base_fee_per_gas(chain.latest_block());
            rpc_success(id, json!(hex_u64(base)))
        }
        "eth_maxPriorityFeePerGas" => rpc_success(id, json!("0x3b9aca00")), // 1 gwei-equivalent default
        "eth_feeHistory" => {
            let chain = chain.lock().await;
            let blocks_count = params_arr
                .first()
                .and_then(|v| v.as_str())
                .and_then(parse_hex_quantity)
                .unwrap_or(1)
                .min(256);
            let newest = match params_arr.get(1).cloned() {
                Some(t) => match resolve_block_tag(&chain, &t) {
                    Some(h) => h,
                    None => return rpc_error(id, -32602, "invalid newestBlock"),
                },
                None => chain.height(),
            };
            let oldest = newest.saturating_sub(blocks_count.saturating_sub(1));
            let mut base_fees: Vec<String> = Vec::new();
            let mut gas_used_ratios: Vec<f64> = Vec::new();
            for h in oldest..=newest {
                if let Some(b) = chain.blocks.get(h as usize) {
                    base_fees.push(hex_u64(chain.next_base_fee_per_gas(b)));
                    gas_used_ratios.push(0.5);
                }
            }
            // Push one extra base fee for the next block (eth_feeHistory contract)
            if let Some(latest) = chain.blocks.last() {
                base_fees.push(hex_u64(chain.next_base_fee_per_gas(latest)));
            }
            rpc_success(
                id,
                json!({
                    "oldestBlock": hex_u64(oldest),
                    "baseFeePerGas": base_fees,
                    "gasUsedRatio": gas_used_ratios,
                }),
            )
        }
        "eth_getBalance" => {
            let Some(addr) = params_arr
                .first()
                .and_then(|v| v.as_str())
                .and_then(parse_hex_address)
            else {
                return rpc_error(id, -32602, "invalid address");
            };
            let chain = chain.lock().await;
            let account = chain.get_account(&addr);
            rpc_success(id, json!(hex_u64(account.balance)))
        }
        "eth_getTransactionCount" => {
            let Some(addr) = params_arr
                .first()
                .and_then(|v| v.as_str())
                .and_then(parse_hex_address)
            else {
                return rpc_error(id, -32602, "invalid address");
            };
            let chain = chain.lock().await;
            let account = chain.get_account(&addr);
            rpc_success(id, json!(hex_u64(account.nonce)))
        }
        "eth_getCode" => {
            let Some(addr) = params_arr
                .first()
                .and_then(|v| v.as_str())
                .and_then(parse_hex_address)
            else {
                return rpc_error(id, -32602, "invalid address");
            };
            let chain = chain.lock().await;
            let code = chain
                .contracts
                .get(&addr)
                .map(|c| hex_bytes(&c.code))
                .unwrap_or_else(|| "0x".to_string());
            rpc_success(id, json!(code))
        }
        "eth_getStorageAt" => {
            let Some(addr) = params_arr
                .first()
                .and_then(|v| v.as_str())
                .and_then(parse_hex_address)
            else {
                return rpc_error(id, -32602, "invalid address");
            };
            let Some(slot_str) = params_arr.get(1).and_then(|v| v.as_str()) else {
                return rpc_error(id, -32602, "invalid storage slot");
            };
            let Some(slot) = parse_storage_slot(slot_str) else {
                return rpc_error(id, -32602, "invalid storage slot (expected 32-byte hex)");
            };
            let chain = chain.lock().await;
            let value = chain
                .contracts
                .get(&addr)
                .and_then(|c| c.storage.get(&slot).cloned())
                .unwrap_or_default();
            rpc_success(id, json!(hex_storage_word(&value)))
        }
        "eth_getBlockByNumber" => {
            let Some(tag) = params_arr.first() else {
                return rpc_error(id, -32602, "missing block tag");
            };
            let full_tx = params_arr.get(1).and_then(|v| v.as_bool()).unwrap_or(false);
            let chain = chain.lock().await;
            let height = match resolve_block_tag(&chain, tag) {
                Some(h) => h,
                None => return rpc_error(id, -32602, "invalid block tag"),
            };
            match chain.blocks.get(height as usize) {
                Some(block) => rpc_success(id, block_to_eth(&chain, block, full_tx)),
                None => rpc_success(id, Value::Null),
            }
        }
        "eth_getBlockByHash" => {
            let Some(hash_hex) = params_arr.first().and_then(|v| v.as_str()) else {
                return rpc_error(id, -32602, "missing hash");
            };
            let Some(target) = parse_hex_hash(hash_hex) else {
                return rpc_error(id, -32602, "invalid block hash");
            };
            let full_tx = params_arr.get(1).and_then(|v| v.as_bool()).unwrap_or(false);
            let chain = chain.lock().await;
            let height = chain.block_hash_to_height.get(&target).copied();
            match height.and_then(|h| chain.blocks.get(h as usize)) {
                Some(block) => rpc_success(id, block_to_eth(&chain, block, full_tx)),
                None => rpc_success(id, Value::Null),
            }
        }
        "eth_getTransactionByHash" => {
            let Some(hash_hex) = params_arr.first().and_then(|v| v.as_str()) else {
                return rpc_error(id, -32602, "missing tx hash");
            };
            let Some(target) = parse_hex_hash(hash_hex) else {
                return rpc_error(id, -32602, "invalid tx hash");
            };
            let chain = chain.lock().await;
            // Try both indexes: native txs are keyed by Transaction::hash(),
            // EVM txs by keccak256(rlp_signed).
            if let Some((height, idx)) = resolve_eth_tx_lookup(&chain, &target)
                && let Some(block) = chain.blocks.get(height as usize)
                && let Some(tx) = block.transactions.get(idx)
            {
                return rpc_success(id, tx_to_eth(&chain, tx, Some((height, idx))));
            }
            rpc_success(id, Value::Null)
        }
        "eth_getTransactionReceipt" => {
            let Some(hash_hex) = params_arr.first().and_then(|v| v.as_str()) else {
                return rpc_error(id, -32602, "missing tx hash");
            };
            let Some(target) = parse_hex_hash(hash_hex) else {
                return rpc_error(id, -32602, "invalid tx hash");
            };
            let chain = chain.lock().await;
            // Receipts are stored keyed by the internal tx.hash(). For an EVM
            // tx, the client only knows the keccak256(rlp) hash, so we go via
            // resolve_eth_tx_lookup → block lookup → real internal hash.
            let internal_hash: Vec<u8> =
                if let Some((height, idx)) = resolve_eth_tx_lookup(&chain, &target) {
                    chain
                        .blocks
                        .get(height as usize)
                        .and_then(|b| b.transactions.get(idx))
                        .map(|tx| tx.hash())
                        .unwrap_or_else(|| target.clone())
                } else {
                    target.clone()
                };
            match chain.get_receipt(&internal_hash) {
                Some(indexed) => rpc_success(id, receipt_to_eth(&chain, &indexed)),
                None => rpc_success(id, Value::Null),
            }
        }
        "eth_getBlockTransactionCountByNumber" => {
            let Some(tag) = params_arr.first() else {
                return rpc_error(id, -32602, "missing block tag");
            };
            let chain = chain.lock().await;
            let height = match resolve_block_tag(&chain, tag) {
                Some(h) => h,
                None => return rpc_error(id, -32602, "invalid block tag"),
            };
            match chain.blocks.get(height as usize) {
                Some(block) => rpc_success(id, json!(hex_u64(block.transactions.len() as u64))),
                None => rpc_success(id, Value::Null),
            }
        }
        "eth_getLogs" => {
            let filter = params_arr.first().cloned().unwrap_or(Value::Null);
            let chain = chain.lock().await;
            let parsed = parse_eth_log_filter(&chain, &filter);
            let parsed = match parsed {
                Ok(f) => f,
                Err(msg) => return rpc_error(id, -32602, &msg),
            };
            // Reject queries that would scan more than MAX_LOG_RANGE blocks
            // (#9). When either bound is unspecified we conservatively use
            // the chain tip / genesis as the open end.
            let chain_tip = chain.height();
            let from = parsed.from_block.unwrap_or(0);
            let to = parsed.to_block.unwrap_or(chain_tip);
            if to >= from && to.saturating_sub(from) > MAX_LOG_RANGE {
                return rpc_error(
                    id,
                    -32602,
                    &format!(
                        "log range too wide: {} blocks (max {})",
                        to.saturating_sub(from).saturating_add(1),
                        MAX_LOG_RANGE
                    ),
                );
            }
            let logs: Vec<Value> = chain
                .query_logs(&parsed)
                .iter()
                .map(|log| log_to_eth(&chain, log))
                .collect();
            rpc_success(id, Value::Array(logs))
        }
        "eth_estimateGas" => rpc_success(id, json!(hex_u64(estimate_eth_gas(&params_arr)))),
        "eth_call" => {
            // params: [{ from, to, gas, gasPrice, value, data }, blockTag]
            let chain = chain.lock().await;
            match dispatch_eth_call(&chain, &params_arr) {
                Ok(result_bytes) => rpc_success(id, json!(hex_bytes(&result_bytes))),
                Err(msg) => rpc_error(id, -32603, &msg),
            }
        }
        "eth_sendTransaction" => rpc_error(
            id,
            -32004,
            "eth_sendTransaction requires a node-side wallet — use eth_sendRawTransaction with a MetaMask-signed payload, or POST /api/tx/submit for native Dilithium-signed txs.",
        ),
        "eth_sendRawTransaction" => {
            let Some(raw_hex) = params_arr.first().and_then(|v| v.as_str()) else {
                return rpc_error(id, -32602, "expected hex-encoded raw tx");
            };
            let stripped = raw_hex.strip_prefix("0x").unwrap_or(raw_hex);
            let raw = match hex::decode(stripped) {
                Ok(b) => b,
                Err(_) => return rpc_error(id, -32602, "invalid hex"),
            };
            let decoded = match crate::vm::evm::decode_raw_eth_tx(&raw) {
                Ok(d) => d,
                Err(e) => return rpc_error(id, -32602, &format!("rlp decode: {}", e)),
            };
            let mut chain_guard = chain.lock().await;
            let chain_id = chain_guard.genesis_config.chain_id.clone();
            let kind = if decoded.to.is_some() {
                TransactionKind::CallEvmContract
            } else {
                TransactionKind::DeployEvmContract
            };
            let to_vec = decoded.to.map(|t| t.to_vec()).unwrap_or_default();
            let tx = Transaction::from_evm_raw(
                &chain_id,
                kind,
                decoded.from.to_vec(),
                to_vec,
                decoded.value,
                decoded.nonce,
                decoded.gas_limit,
                decoded.max_fee_per_gas,
                decoded.max_priority_fee_per_gas,
                decoded.data.clone(),
                raw,
            );
            match chain_guard.add_transaction(tx) {
                Ok(()) => rpc_success(id, json!(format!("0x{}", hex::encode(decoded.tx_hash)))),
                Err(e) => rpc_error(id, -32003, &format!("tx rejected: {}", e)),
            }
        }
        _ => rpc_error(id, -32601, &format!("method {} not supported", method)),
    }
}

/// Run an `eth_call` against revm read-only. Builds an EVM state view from
/// the canonical chain state and invokes `vm::evm::call`. Discards any state
/// changes — only the return data is bubbled up.
fn dispatch_eth_call(chain: &Blockchain, params: &[Value]) -> Result<Vec<u8>, String> {
    let call_obj = params
        .first()
        .and_then(|v| v.as_object())
        .ok_or_else(|| "missing call object".to_string())?;
    let from = call_obj
        .get("from")
        .and_then(|v| v.as_str())
        .and_then(parse_hex_address)
        .unwrap_or_else(|| vec![0u8; 20]);
    let to = call_obj
        .get("to")
        .and_then(|v| v.as_str())
        .and_then(parse_hex_address)
        .ok_or_else(|| "missing 'to'".to_string())?;
    let value = call_obj
        .get("value")
        .and_then(|v| v.as_str())
        .and_then(parse_hex_quantity)
        .unwrap_or(0);
    let gas_limit = call_obj
        .get("gas")
        .and_then(|v| v.as_str())
        .and_then(parse_hex_quantity)
        .unwrap_or(30_000_000);
    let gas_price = call_obj
        .get("gasPrice")
        .and_then(|v| v.as_str())
        .and_then(parse_hex_quantity)
        .unwrap_or(0);
    let data_hex = call_obj
        .get("data")
        .or_else(|| call_obj.get("input"))
        .and_then(|v| v.as_str())
        .unwrap_or("0x");
    let data = parse_hex_hash(data_hex).unwrap_or_default();

    let mut from_arr = [0u8; 20];
    if from.len() == 20 {
        from_arr.copy_from_slice(&from);
    }
    let mut to_arr = [0u8; 20];
    if to.len() == 20 {
        to_arr.copy_from_slice(&to);
    }

    let mut view = crate::vm::evm::EvmStateView::new();
    for (addr, account) in &chain.accounts {
        if addr.len() != 20 {
            continue;
        }
        let mut k = [0u8; 20];
        k.copy_from_slice(addr);
        view.insert_account(k, account.balance, account.nonce);
    }
    for (addr, contract) in &chain.contracts {
        if addr.len() != 20 {
            continue;
        }
        let mut k = [0u8; 20];
        k.copy_from_slice(addr);
        view.insert_contract(k, contract.clone());
    }
    let base_fee = chain.next_base_fee_per_gas(chain.latest_block());
    let outcome = crate::vm::evm::call(
        view,
        from_arr,
        to_arr,
        &data,
        value,
        gas_limit,
        gas_price.max(base_fee),
        chain.height(),
        base_fee,
        crate::core::chain::DEFAULT_BLOCK_GAS_LIMIT,
    )
    .map_err(|e| format!("evm error: {}", e))?;
    if !outcome.success {
        return Err(format!(
            "eth_call reverted: {}",
            hex::encode(&outcome.return_data)
        ));
    }
    Ok(outcome.return_data)
}

fn parse_eth_log_filter(chain: &Blockchain, value: &Value) -> Result<LogFilter, String> {
    let map = match value.as_object() {
        Some(m) => m,
        None => return Ok(LogFilter::default()),
    };

    let address = match map.get("address") {
        Some(Value::String(addr)) => parse_hex_address(addr).map(|a| vec![a]),
        Some(Value::Array(arr)) => {
            let mut out = Vec::new();
            for item in arr {
                if let Some(addr) = item.as_str().and_then(parse_hex_address) {
                    out.push(addr);
                }
            }
            if out.is_empty() { None } else { Some(out) }
        }
        _ => None,
    };
    // Only the first address is honored by query_logs (single-contract filter).
    let contract = address.and_then(|mut v| v.pop());

    let topics = match map.get("topics") {
        Some(Value::Array(arr)) => {
            let mut positional: Vec<Option<Vec<u8>>> = Vec::with_capacity(arr.len());
            for item in arr {
                match item {
                    Value::Null => positional.push(None),
                    Value::String(hex_str) => positional.push(parse_hex_hash(hex_str)),
                    Value::Array(_) => {
                        // Topic OR-list — query_logs only supports single value per position.
                        // Take the first entry as a best-effort match.
                        if let Some(first) = item.as_array().and_then(|a| a.first()) {
                            match first {
                                Value::String(s) => positional.push(parse_hex_hash(s)),
                                _ => positional.push(None),
                            }
                        } else {
                            positional.push(None);
                        }
                    }
                    _ => positional.push(None),
                }
            }
            if positional.is_empty() {
                None
            } else {
                Some(positional)
            }
        }
        _ => None,
    };

    let from_block = map
        .get("fromBlock")
        .and_then(|v| resolve_block_tag(chain, v));
    let to_block = map.get("toBlock").and_then(|v| resolve_block_tag(chain, v));
    let limit = map
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    Ok(LogFilter {
        contract,
        topic: None,
        topics,
        from_block,
        to_block,
        limit,
    })
}

/// Top-level handler for a JSON-RPC request body. Supports single requests and batches.
pub async fn handle(chain: Arc<Mutex<Blockchain>>, body: &[u8]) -> Value {
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return rpc_error(Value::Null, -32700, "Parse error");
        }
    };

    if let Some(batch) = parsed.as_array() {
        let mut responses = Vec::with_capacity(batch.len());
        for entry in batch {
            responses.push(dispatch(&chain, entry).await);
        }
        Value::Array(responses)
    } else {
        dispatch(&chain, &parsed).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_evm_transfer_raw(nonce: u64) -> (String, String, [u8; 20]) {
        use alloy_consensus::transaction::SignerRecoverable;
        use alloy_consensus::{SignableTransaction, TxEip1559};
        use alloy_eips::eip2718::Encodable2718;
        use alloy_primitives::{Address, Bytes, TxKind, U256};
        use alloy_signer::SignerSync;
        use alloy_signer_local::PrivateKeySigner;

        let signer: PrivateKeySigner =
            "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                .parse()
                .unwrap();
        let from_addr = signer.address();
        let tx = TxEip1559 {
            chain_id: 1,
            nonce,
            max_fee_per_gas: 100_000,
            max_priority_fee_per_gas: 1,
            gas_limit: 100_000,
            to: TxKind::Call(Address::ZERO),
            value: U256::from(1u64),
            input: Bytes::new(),
            access_list: Default::default(),
        };
        let sig_hash = tx.signature_hash();
        let signature = signer.sign_hash_sync(&sig_hash).unwrap();
        let signed = tx.into_signed(signature);
        let envelope: alloy_consensus::TxEnvelope = signed.into();
        assert_eq!(envelope.recover_signer().unwrap(), from_addr);

        let tx_hash = format!("0x{}", hex::encode(envelope.tx_hash()));
        let raw = envelope.encoded_2718();
        let mut from = [0u8; 20];
        from.copy_from_slice(from_addr.as_slice());
        (format!("0x{}", hex::encode(raw)), tx_hash, from)
    }

    #[test]
    fn numeric_chain_id_is_stable_and_positive() {
        let id_a = numeric_chain_id("curs3d-testnet");
        let id_b = numeric_chain_id("curs3d-testnet");
        assert_eq!(id_a, id_b, "must be deterministic");
        assert!(id_a > 0);
        assert!(id_a < (1u64 << 31));
        // Different chains -> different ids
        assert_ne!(id_a, numeric_chain_id("curs3d-mainnet"));
    }

    #[test]
    fn parse_hex_quantity_handles_prefixed_and_bare_hex() {
        assert_eq!(parse_hex_quantity("0x10"), Some(16));
        assert_eq!(parse_hex_quantity("0x"), Some(0));
        assert_eq!(parse_hex_quantity("ff"), Some(255));
        assert_eq!(parse_hex_quantity("zz"), None);
    }

    #[test]
    fn parse_hex_address_accepts_eth_and_curs3d_styles() {
        let canonical = "00112233445566778899aabbccddeeff00112233";
        let bytes = parse_hex_address(canonical).unwrap();
        assert_eq!(bytes.len(), 20);
        let with_prefix = format!("0x{}", canonical);
        assert_eq!(parse_hex_address(&with_prefix).unwrap(), bytes);
        let cur_prefix = format!("CUR{}", canonical);
        assert_eq!(parse_hex_address(&cur_prefix).unwrap(), bytes);
        // Wrong length -> None
        assert!(parse_hex_address("00").is_none());
    }

    #[test]
    fn hex_helpers_are_zero_padded_correctly() {
        assert_eq!(hex_u64(0), "0x0");
        assert_eq!(hex_u64(255), "0xff");
        assert_eq!(hex_bytes(&[]), "0x");
        assert_eq!(hex_bytes(&[0xab, 0xcd]), "0xabcd");
    }

    #[test]
    fn parse_storage_slot_pads_left_to_32_bytes() {
        // Empty / "0x" → 32 zero bytes
        assert_eq!(parse_storage_slot("0x").unwrap(), vec![0u8; 32]);
        // Single byte → padded to 32
        let one = parse_storage_slot("0x01").unwrap();
        assert_eq!(one.len(), 32);
        assert_eq!(one[31], 0x01);
        assert_eq!(&one[..31], &[0u8; 31][..]);
        // Odd-length hex (3 chars) is left-padded to 4 then to 32
        let three = parse_storage_slot("0x123").unwrap();
        assert_eq!(three.len(), 32);
        assert_eq!(three[30..], [0x01, 0x23]);
        // Full 32-byte slot returned as-is
        let full = "0x".to_string() + &"ab".repeat(32);
        let parsed = parse_storage_slot(&full).unwrap();
        assert_eq!(parsed.len(), 32);
        assert!(parsed.iter().all(|b| *b == 0xab));
        // > 32 bytes rejected
        let too_long = "0x".to_string() + &"00".repeat(33);
        assert!(parse_storage_slot(&too_long).is_none());
        // Non-hex rejected
        assert!(parse_storage_slot("0xZZ").is_none());
    }

    #[tokio::test]
    async fn dispatch_unknown_method_returns_method_not_found() {
        let chain = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::core::chain::Blockchain::new(),
        ));
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "wat_clientVersion" });
        let resp = dispatch(&chain, &req).await;
        assert_eq!(resp["id"], json!(1));
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn dispatch_eth_block_number_returns_height_hex() {
        let chain = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::core::chain::Blockchain::new(),
        ));
        let req = json!({ "jsonrpc": "2.0", "id": 7, "method": "eth_blockNumber" });
        let resp = dispatch(&chain, &req).await;
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 7);
        // Genesis-only chain → height 0 → "0x0"
        assert_eq!(resp["result"], "0x0");
    }

    #[tokio::test]
    async fn dispatch_eth_send_raw_transaction_rejects_garbage_rlp() {
        // After v4 hard-fork, eth_sendRawTransaction tries to RLP-decode the
        // payload. Garbage like "0xdead" should still fail, but with an
        // RLP-decode error rather than the legacy "use Dilithium" hint.
        let chain = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::core::chain::Blockchain::new(),
        ));
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendRawTransaction",
            "params": ["0xdead"]
        });
        let resp = dispatch(&chain, &req).await;
        let err_msg = resp["error"]["message"].as_str().unwrap();
        assert!(
            err_msg.to_lowercase().contains("rlp") || err_msg.to_lowercase().contains("decode"),
            "expected an rlp/decode error, got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_eth_send_raw_transaction_admits_signed_evm_tx() {
        // We sign a tiny EIP-1559 tx using a fresh secp256k1 key (via the
        // alloy_signer test harness) and submit it via eth_sendRawTransaction.
        // The tx should land in the chain's mempool and the response should
        // be the EVM-style tx hash. Because we build the chain fresh with no
        // accounts, the admission check needs the signer address to have
        // balance >= total_fee_cap; we credit it first.
        use alloy_consensus::transaction::SignerRecoverable;
        use alloy_consensus::{SignableTransaction, TxEip1559};
        use alloy_primitives::{Bytes, TxKind, U256, hex as alloy_hex};
        use alloy_signer::SignerSync;
        use alloy_signer_local::PrivateKeySigner;
        let _ = alloy_hex::const_decode_to_array::<32>;

        // Pick a deterministic key — same key every run.
        let signer: PrivateKeySigner =
            "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                .parse()
                .unwrap();
        let from_addr = signer.address();

        let tx = TxEip1559 {
            chain_id: 1,
            nonce: 0,
            max_fee_per_gas: 100_000,
            max_priority_fee_per_gas: 1,
            gas_limit: 100_000,
            to: TxKind::Call(alloy_primitives::Address::ZERO),
            value: U256::from(1u64),
            input: Bytes::new(),
            access_list: Default::default(),
        };
        let sig_hash = tx.signature_hash();
        let signature = signer.sign_hash_sync(&sig_hash).unwrap();
        let signed = tx.into_signed(signature);
        let envelope: alloy_consensus::TxEnvelope = signed.into();
        let recovered = envelope.recover_signer().unwrap();
        assert_eq!(recovered, from_addr);
        let raw = alloy_eips::eip2718::Encodable2718::encoded_2718(&envelope);
        let raw_hex = format!("0x{}", hex::encode(&raw));

        let chain_mutex = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::core::chain::Blockchain::new(),
        ));
        // Fund the signer's CURS3D-side account so the chain admits the tx.
        {
            let mut chain = chain_mutex.lock().await;
            let mut from_bytes = [0u8; 20];
            from_bytes.copy_from_slice(from_addr.as_slice());
            let acct = chain.accounts.entry(from_bytes.to_vec()).or_default();
            acct.balance = u64::MAX / 4;
        }

        let req = json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "eth_sendRawTransaction",
            "params": [raw_hex]
        });
        let resp = dispatch(&chain_mutex, &req).await;
        let result = resp
            .get("result")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        assert!(result.is_some(), "expected success result, got: {}", resp);
        // mempool should now contain exactly 1 tx
        let chain = chain_mutex.lock().await;
        assert_eq!(chain.pending_transactions.len(), 1);
        let admitted = &chain.pending_transactions[0];
        assert!(admitted.is_evm());
        assert_eq!(admitted.from.len(), 20);
    }

    #[tokio::test]
    async fn evm_wire_hash_resolves_transaction_and_receipt_after_mining() {
        let (raw_hex, eth_hash, from) = signed_evm_transfer_raw(0);
        let chain_mutex = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::core::chain::Blockchain::new(),
        ));

        {
            let mut chain = chain_mutex.lock().await;
            let acct = chain.accounts.entry(from.to_vec()).or_default();
            acct.balance = u64::MAX / 4;
        }

        let send = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendRawTransaction",
            "params": [raw_hex]
        });
        let send_resp = dispatch(&chain_mutex, &send).await;
        assert_eq!(send_resp["result"], eth_hash);

        {
            let validator = crate::crypto::dilithium::KeyPair::generate();
            let mut chain = chain_mutex.lock().await;
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        let get_tx = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "eth_getTransactionByHash",
            "params": [eth_hash]
        });
        let tx_resp = dispatch(&chain_mutex, &get_tx).await;
        assert_eq!(tx_resp["result"]["hash"], send_resp["result"]);
        assert_ne!(tx_resp["result"]["blockHash"], Value::Null);
        assert_eq!(tx_resp["result"]["blockNumber"], "0x1");

        let get_receipt = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "eth_getTransactionReceipt",
            "params": [send_resp["result"].clone()]
        });
        let receipt_resp = dispatch(&chain_mutex, &get_receipt).await;
        assert_eq!(
            receipt_resp["result"]["transactionHash"],
            send_resp["result"]
        );
        assert_ne!(receipt_resp["result"]["blockHash"], Value::Null);
        assert_eq!(receipt_resp["result"]["blockNumber"], "0x1");
        assert_eq!(
            receipt_resp["result"]["from"],
            format!("0x{}", hex::encode(from))
        );
    }

    #[tokio::test]
    async fn eth_storage_and_estimate_gas_are_tooling_safe() {
        let chain = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::core::chain::Blockchain::new(),
        ));
        let missing_storage = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getStorageAt",
            "params": [
                "0x0000000000000000000000000000000000000001",
                "0x0",
                "latest"
            ]
        });
        let storage_resp = dispatch(&chain, &missing_storage).await;
        assert_eq!(storage_resp["result"], format!("0x{}", "00".repeat(32)));

        let estimate_deploy = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "eth_estimateGas",
            "params": [{"data": "0x60006000"}]
        });
        let estimate_resp = dispatch(&chain, &estimate_deploy).await;
        let gas = estimate_resp["result"]
            .as_str()
            .and_then(parse_hex_quantity);
        assert!(gas.unwrap() > 21_000);
    }

    #[tokio::test]
    async fn handle_supports_batch_requests() {
        let chain = std::sync::Arc::new(tokio::sync::Mutex::new(
            crate::core::chain::Blockchain::new(),
        ));
        let body = br#"[
            {"jsonrpc":"2.0","id":1,"method":"eth_chainId"},
            {"jsonrpc":"2.0","id":2,"method":"eth_blockNumber"}
        ]"#;
        let resp = handle(chain, body).await;
        let arr = resp.as_array().expect("batch should return array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], 1);
        assert_eq!(arr[1]["id"], 2);
    }
}
