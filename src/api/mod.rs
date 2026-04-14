use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Semaphore, broadcast, mpsc};

use crate::core::block::Block;
use crate::core::chain::Blockchain;
use crate::core::receipt::{IndexedLogEntry, IndexedReceipt, LogFilter};
use crate::core::state_proof::{AccountProof, StorageProof};
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::hash;
use crate::network::NetworkMessage;

const MAX_API_BODY_BYTES: usize = 1024 * 1024;
const MAX_HTTP_CONNECTIONS: usize = 128;
const RATE_LIMIT_GET: usize = 60;
const RATE_LIMIT_POST: usize = 10;
const RATE_LIMIT_WINDOW_SECS: u64 = 60;
const RATE_LIMIT_CLEANUP_SECS: u64 = 120;
const RATE_LIMIT_CLEANUP_INTERVAL: u64 = 100;
const FAUCET_AMOUNT: u64 = 100_000_000;
const FAUCET_COOLDOWN_SECS: u64 = 3600;
static API_START_TIME: OnceLock<Instant> = OnceLock::new();

type RateLimiterMap = Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>;
type FaucetCooldownMap = Arc<Mutex<HashMap<Vec<u8>, Instant>>>;

struct RequestContext {
    peer_ip: IpAddr,
    rate_limiter: RateLimiterMap,
    request_counter: Arc<AtomicU64>,
    faucet_cooldowns: FaucetCooldownMap,
}

// ─── API Response Types ──────────────────────────────────────────────

#[derive(Serialize)]
struct ApiResponse<T: Serialize> {
    ok: bool,
    data: Option<T>,
    error: Option<String>,
}

fn json_ok<T: Serialize>(data: T) -> Response<Full<Bytes>> {
    let resp = ApiResponse {
        ok: true,
        data: Some(data),
        error: None,
    };
    let body = serde_json::to_string(&resp).unwrap_or_default();
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json");
    builder = with_cors_headers(builder);
    builder.body(Full::new(Bytes::from(body))).unwrap()
}

fn json_err(status: StatusCode, msg: &str) -> Response<Full<Bytes>> {
    let resp: ApiResponse<()> = ApiResponse {
        ok: false,
        data: None,
        error: Some(msg.to_string()),
    };
    let body = serde_json::to_string(&resp).unwrap_or_default();
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", "application/json");
    builder = with_cors_headers(builder);
    builder.body(Full::new(Bytes::from(body))).unwrap()
}

fn text_response(status: StatusCode, content_type: &str, body: String) -> Response<Full<Bytes>> {
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", content_type);
    builder = with_cors_headers(builder);
    builder.body(Full::new(Bytes::from(body))).unwrap()
}

fn cors_preflight() -> Response<Full<Bytes>> {
    if cors_allow_origin().is_none() {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Full::new(Bytes::new()))
            .unwrap();
    }
    let mut builder = Response::builder().status(StatusCode::NO_CONTENT);
    builder = with_cors_headers(builder);
    builder.body(Full::new(Bytes::new())).unwrap()
}

fn cors_allow_origin() -> Option<String> {
    std::env::var("CURS3D_API_ALLOW_ORIGIN")
        .ok()
        .filter(|value| !value.is_empty())
}

fn with_cors_headers(builder: hyper::http::response::Builder) -> hyper::http::response::Builder {
    let Some(origin) = cors_allow_origin() else {
        return builder;
    };
    builder
        .header("Access-Control-Allow-Origin", origin)
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header(
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization",
        )
}

// ─── Serializable API Structs ────────────────────────────────────────

#[derive(Serialize)]
struct ApiStatus {
    chain_id: String,
    chain_name: String,
    epoch: u64,
    epoch_start_height: u64,
    height: u64,
    finalized_height: u64,
    latest_hash: String,
    genesis_hash: String,
    pending_transactions: usize,
    active_validators: usize,
    protocol_version: u32,
}

#[derive(Serialize)]
struct ApiBlock {
    height: u64,
    hash: String,
    prev_hash: String,
    timestamp: i64,
    validator: String,
    tx_count: usize,
    state_root: String,
    merkle_root: String,
    transactions: Vec<ApiTransaction>,
}

#[derive(Serialize)]
struct ApiBlockSummary {
    height: u64,
    hash: String,
    prev_hash: String,
    timestamp: i64,
    validator: String,
    tx_count: usize,
}

#[derive(Serialize)]
struct ApiTransaction {
    hash: String,
    kind: String,
    from: String,
    to: String,
    amount: u64,
    fee: u64,
    max_fee_per_gas: u64,
    max_priority_fee_per_gas: u64,
    gas_limit: u64,
    nonce: u64,
    timestamp: i64,
}

#[derive(Serialize)]
struct ApiAccount {
    address: String,
    balance: u64,
    nonce: u64,
    staked_balance: u64,
}

#[derive(Serialize)]
struct ApiAccountProof {
    address: String,
    leaf_index: usize,
    leaf_hash: String,
    proof: Vec<String>,
    state_root: String,
    balance: u64,
    nonce: u64,
    staked_balance: u64,
    validator_active_from_height: u64,
    jailed_until_height: u64,
}

#[derive(Serialize)]
struct ApiStorageProof {
    contract_address: String,
    contract_code_hash: String,
    contract_owner: String,
    key: String,
    value: String,
    storage_leaf_index: usize,
    storage_leaf_hash: String,
    storage_proof: Vec<String>,
    storage_root: String,
    contract_leaf_index: usize,
    contract_leaf_hash: String,
    contract_proof: Vec<String>,
    state_root: String,
}

#[derive(Serialize)]
struct ApiValidator {
    address: String,
    public_key: String,
    stake: u64,
}

#[derive(Serialize)]
struct ApiReceipt {
    tx_hash: String,
    block_height: u64,
    tx_index: usize,
    success: bool,
    gas_used: u64,
    effective_gas_price: u64,
    priority_fee_paid: u64,
    base_fee_burned: u64,
    gas_refunded: u64,
    contract_address: Option<String>,
    return_data: String,
    logs: Vec<ApiLogEntry>,
}

#[derive(Serialize)]
struct ApiLogEntry {
    block_height: u64,
    tx_index: usize,
    log_index: usize,
    tx_hash: String,
    contract: String,
    topics: Vec<String>,
    data: String,
}

#[derive(Serialize)]
struct ApiHealth {
    ok: bool,
    chain_id: String,
    height: u64,
    finalized_height: u64,
    latest_block_timestamp: i64,
    latest_block_age_secs: i64,
    pending_transactions: usize,
}

// ─── Converters ──────────────────────────────────────────────────────

fn block_to_api(block: &Block) -> ApiBlock {
    ApiBlock {
        height: block.header.height,
        hash: hex::encode(&block.hash),
        prev_hash: hex::encode(&block.header.prev_hash),
        timestamp: block.header.timestamp,
        validator: hex::encode(&block.header.validator_public_key),
        tx_count: block.transactions.len(),
        state_root: hex::encode(&block.header.state_root),
        merkle_root: hex::encode(&block.header.merkle_root),
        transactions: block.transactions.iter().map(tx_to_api).collect(),
    }
}

fn block_to_summary(block: &Block) -> ApiBlockSummary {
    ApiBlockSummary {
        height: block.header.height,
        hash: hex::encode(&block.hash),
        prev_hash: hex::encode(&block.header.prev_hash),
        timestamp: block.header.timestamp,
        validator: hex::encode(&block.header.validator_public_key),
        tx_count: block.transactions.len(),
    }
}

fn tx_to_api(tx: &Transaction) -> ApiTransaction {
    ApiTransaction {
        hash: tx.hash_hex(),
        kind: match tx.kind {
            TransactionKind::Transfer => "transfer".to_string(),
            TransactionKind::Stake => "stake".to_string(),
            TransactionKind::Unstake => "unstake".to_string(),
            TransactionKind::Coinbase => "coinbase".to_string(),
            TransactionKind::DeployContract => "deploy_contract".to_string(),
            TransactionKind::CallContract => "call_contract".to_string(),
        },
        from: hex::encode(&tx.from),
        to: hex::encode(&tx.to),
        amount: tx.amount,
        fee: tx.fee,
        max_fee_per_gas: tx.max_fee_per_gas(),
        max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
        gas_limit: tx.gas_limit,
        nonce: tx.nonce,
        timestamp: tx.timestamp,
    }
}

fn account_proof_to_api(proof: AccountProof) -> ApiAccountProof {
    ApiAccountProof {
        address: hex::encode(proof.address),
        leaf_index: proof.leaf_index,
        leaf_hash: hex::encode(proof.leaf_hash),
        proof: proof.proof.into_iter().map(hex::encode).collect(),
        state_root: hex::encode(proof.state_root),
        balance: proof.state.balance,
        nonce: proof.state.nonce,
        staked_balance: proof.state.staked_balance,
        validator_active_from_height: proof.state.validator_active_from_height,
        jailed_until_height: proof.state.jailed_until_height,
    }
}

fn storage_proof_to_api(proof: StorageProof) -> ApiStorageProof {
    ApiStorageProof {
        contract_address: hex::encode(proof.contract_address),
        contract_code_hash: hex::encode(proof.contract_code_hash),
        contract_owner: hex::encode(proof.contract_owner),
        key: hex::encode(proof.key),
        value: hex::encode(proof.value),
        storage_leaf_index: proof.storage_leaf_index,
        storage_leaf_hash: hex::encode(proof.storage_leaf_hash),
        storage_proof: proof.storage_proof.into_iter().map(hex::encode).collect(),
        storage_root: hex::encode(proof.storage_root),
        contract_leaf_index: proof.contract_leaf_index,
        contract_leaf_hash: hex::encode(proof.contract_leaf_hash),
        contract_proof: proof.contract_proof.into_iter().map(hex::encode).collect(),
        state_root: hex::encode(proof.state_root),
    }
}

fn indexed_log_to_api(entry: IndexedLogEntry) -> ApiLogEntry {
    ApiLogEntry {
        block_height: entry.block_height,
        tx_index: entry.tx_index,
        log_index: entry.log_index,
        tx_hash: hex::encode(entry.tx_hash),
        contract: hex::encode(entry.contract),
        topics: entry.topics.into_iter().map(hex::encode).collect(),
        data: hex::encode(entry.data),
    }
}

fn indexed_receipt_to_api(receipt: IndexedReceipt) -> ApiReceipt {
    let tx_hash_hex = hex::encode(&receipt.tx_hash);
    ApiReceipt {
        tx_hash: tx_hash_hex.clone(),
        block_height: receipt.block_height,
        tx_index: receipt.tx_index,
        success: receipt.receipt.success,
        gas_used: receipt.receipt.gas_used,
        effective_gas_price: receipt.receipt.effective_gas_price,
        priority_fee_paid: receipt.receipt.priority_fee_paid,
        base_fee_burned: receipt.receipt.base_fee_burned,
        gas_refunded: receipt.receipt.gas_refunded,
        contract_address: receipt.receipt.contract_address.map(hex::encode),
        return_data: hex::encode(receipt.receipt.return_data),
        logs: receipt
            .receipt
            .logs
            .into_iter()
            .enumerate()
            .map(|(log_index, log)| ApiLogEntry {
                block_height: receipt.block_height,
                tx_index: receipt.tx_index,
                log_index,
                tx_hash: tx_hash_hex.clone(),
                contract: hex::encode(log.contract),
                topics: log.topics.into_iter().map(hex::encode).collect(),
                data: hex::encode(log.data),
            })
            .collect(),
    }
}

fn api_token() -> Option<String> {
    std::env::var("CURS3D_API_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
}

fn enforce_api_auth(req: &Request<Incoming>) -> Option<Response<Full<Bytes>>> {
    let token = api_token()?;

    let expected = format!("Bearer {}", token);
    match req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        Some(actual) if actual == expected => None,
        _ => Some(json_err(
            StatusCode::UNAUTHORIZED,
            "missing or invalid API bearer token",
        )),
    }
}

// ─── Request Router ──────────────────────────────────────────────────

async fn check_rate_limit(
    peer_ip: IpAddr,
    method: &Method,
    rate_limiter: &RateLimiterMap,
    request_counter: &AtomicU64,
) -> Option<Response<Full<Bytes>>> {
    let now = Instant::now();
    let max_requests = if *method == Method::POST {
        RATE_LIMIT_POST
    } else {
        RATE_LIMIT_GET
    };

    let mut limiter = rate_limiter.lock().await;

    // Periodic cleanup to prevent memory leaks
    let count = request_counter.fetch_add(1, Ordering::Relaxed) + 1;
    if count.is_multiple_of(RATE_LIMIT_CLEANUP_INTERVAL) {
        limiter.retain(|_, timestamps| {
            timestamps.retain(|ts| now.duration_since(*ts).as_secs() < RATE_LIMIT_CLEANUP_SECS);
            !timestamps.is_empty()
        });
    }

    let timestamps = limiter.entry(peer_ip).or_default();
    timestamps.retain(|ts| now.duration_since(*ts).as_secs() < RATE_LIMIT_WINDOW_SECS);

    if timestamps.len() >= max_requests {
        return Some(json_err(
            StatusCode::TOO_MANY_REQUESTS,
            &format!(
                "rate limit exceeded: max {} requests per minute",
                max_requests
            ),
        ));
    }

    timestamps.push(now);
    None
}

async fn handle_request(
    req: Request<Incoming>,
    chain: Arc<Mutex<Blockchain>>,
    event_tx: broadcast::Sender<String>,
    outbound_tx: mpsc::Sender<NetworkMessage>,
    ctx: RequestContext,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    if req.method() == Method::OPTIONS {
        return Ok(cors_preflight());
    }

    // Rate limit check before any processing
    if let Some(response) = check_rate_limit(
        ctx.peer_ip,
        req.method(),
        &ctx.rate_limiter,
        &ctx.request_counter,
    )
    .await
    {
        return Ok(response);
    }

    let faucet_cooldowns = ctx.faucet_cooldowns;

    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    match (method, segments.as_slice()) {
        // GET /api/healthz
        (Method::GET, ["api", "healthz"]) => {
            let chain = chain.lock().await;
            let latest_ts = chain.latest_block().header.timestamp;
            let age = chrono::Utc::now().timestamp().saturating_sub(latest_ts);
            Ok(json_ok(ApiHealth {
                ok: true,
                chain_id: chain.chain_id().to_string(),
                height: chain.height(),
                finalized_height: chain.finalized_height(),
                latest_block_timestamp: latest_ts,
                latest_block_age_secs: age,
                pending_transactions: chain.pending_transactions.len(),
            }))
        }

        // GET /api/metrics
        (Method::GET, ["api", "metrics"]) => {
            let chain = chain.lock().await;
            let uptime = API_START_TIME
                .get()
                .map(|start| start.elapsed().as_secs())
                .unwrap_or_default();
            let body = format!(
                concat!(
                    "# TYPE curs3d_uptime_seconds counter\n",
                    "curs3d_uptime_seconds {}\n",
                    "# TYPE curs3d_chain_height gauge\n",
                    "curs3d_chain_height {}\n",
                    "# TYPE curs3d_finalized_height gauge\n",
                    "curs3d_finalized_height {}\n",
                    "# TYPE curs3d_pending_transactions gauge\n",
                    "curs3d_pending_transactions {}\n",
                    "# TYPE curs3d_active_validators gauge\n",
                    "curs3d_active_validators {}\n",
                    "# TYPE curs3d_accounts_total gauge\n",
                    "curs3d_accounts_total {}\n",
                    "# TYPE curs3d_contracts_total gauge\n",
                    "curs3d_contracts_total {}\n",
                    "# TYPE curs3d_receipts_total gauge\n",
                    "curs3d_receipts_total {}\n",
                    "# TYPE curs3d_logs_total gauge\n",
                    "curs3d_logs_total {}\n",
                    "# TYPE curs3d_base_fee_per_gas gauge\n",
                    "curs3d_base_fee_per_gas {}\n",
                ),
                uptime,
                chain.height(),
                chain.finalized_height(),
                chain.pending_transactions.len(),
                chain.active_validator_count(),
                chain.accounts.len(),
                chain.contracts.len(),
                chain.receipts.len(),
                chain.log_index.len(),
                chain.current_base_fee_per_gas(),
            );
            Ok(text_response(
                StatusCode::OK,
                "text/plain; version=0.0.4",
                body,
            ))
        }

        // GET /api/status
        (Method::GET, ["api", "status"]) => {
            let chain = chain.lock().await;
            Ok(json_ok(ApiStatus {
                chain_id: chain.chain_id().to_string(),
                chain_name: chain.genesis_config.chain_name.clone(),
                epoch: chain.current_epoch(),
                epoch_start_height: chain.current_epoch_start_height(),
                height: chain.height(),
                finalized_height: chain.finalized_height(),
                latest_hash: hex::encode(chain.latest_hash()),
                genesis_hash: hex::encode(chain.genesis_hash()),
                pending_transactions: chain.pending_transactions.len(),
                active_validators: chain.active_validator_count(),
                protocol_version: chain.protocol_version_at_height(chain.height()),
            }))
        }

        // GET /api/block/:height
        (Method::GET, ["api", "block", height_str]) => {
            let height: u64 = match height_str.parse() {
                Ok(h) => h,
                Err(_) => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid height")),
            };
            let chain = chain.lock().await;
            match chain.blocks.get(height as usize) {
                Some(block) => Ok(json_ok(block_to_api(block))),
                None => Ok(json_err(StatusCode::NOT_FOUND, "block not found")),
            }
        }

        // GET /api/blocks?from=0&limit=20
        (Method::GET, ["api", "blocks"]) => {
            let query = req.uri().query().unwrap_or("");
            let params: Vec<(&str, &str)> =
                query.split('&').filter_map(|p| p.split_once('=')).collect();

            let from: u64 = params
                .iter()
                .find(|(k, _)| *k == "from")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            let limit: usize = params
                .iter()
                .find(|(k, _)| *k == "limit")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(20)
                .min(100);

            let chain = chain.lock().await;
            let height = chain.height();
            let start = if from == 0 && height >= limit as u64 {
                height - limit as u64 + 1
            } else {
                from
            };

            let blocks: Vec<ApiBlockSummary> = (start..=height)
                .rev()
                .take(limit)
                .filter_map(|h| chain.blocks.get(h as usize).map(block_to_summary))
                .collect();

            Ok(json_ok(blocks))
        }

        // GET /api/account/:address
        (Method::GET, ["api", "account", addr_hex]) => {
            let addr_clean = addr_hex.strip_prefix("CUR").unwrap_or(addr_hex);
            let address = match hex::decode(addr_clean) {
                Ok(a) if a.len() == hash::ADDRESS_LEN => a,
                _ => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid address")),
            };
            let chain = chain.lock().await;
            let state = chain.get_account(&address);
            Ok(json_ok(ApiAccount {
                address: hex::encode(&address),
                balance: state.balance,
                nonce: state.nonce,
                staked_balance: state.staked_balance,
            }))
        }

        // GET /api/account/:address/proof
        (Method::GET, ["api", "account", addr_hex, "proof"]) => {
            let addr_clean = addr_hex.strip_prefix("CUR").unwrap_or(addr_hex);
            let address = match hex::decode(addr_clean) {
                Ok(a) if a.len() == hash::ADDRESS_LEN => a,
                _ => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid address")),
            };
            let chain = chain.lock().await;
            match chain.get_account_proof(&address) {
                Some(proof) => Ok(json_ok(account_proof_to_api(proof))),
                None => Ok(json_err(StatusCode::NOT_FOUND, "account proof not found")),
            }
        }

        // GET /api/contract/:address/storage/:key/proof
        (Method::GET, ["api", "contract", contract_hex, "storage", key_hex, "proof"]) => {
            let contract_address = match hex::decode(contract_hex) {
                Ok(a) if a.len() == hash::ADDRESS_LEN => a,
                _ => {
                    return Ok(json_err(
                        StatusCode::BAD_REQUEST,
                        "invalid contract address",
                    ));
                }
            };
            let key = match hex::decode(key_hex) {
                Ok(value) => value,
                Err(_) => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid storage key")),
            };
            let chain = chain.lock().await;
            match chain.get_storage_proof(&contract_address, &key) {
                Some(proof) => Ok(json_ok(storage_proof_to_api(proof))),
                None => Ok(json_err(StatusCode::NOT_FOUND, "storage proof not found")),
            }
        }

        // GET /api/tx/:hash
        (Method::GET, ["api", "tx", tx_hash]) => {
            let target = match hex::decode(tx_hash) {
                Ok(h) => h,
                Err(_) => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid tx hash")),
            };
            let chain = chain.lock().await;
            for block in chain.blocks.iter().rev() {
                for tx in &block.transactions {
                    if tx.hash() == target {
                        return Ok(json_ok(tx_to_api(tx)));
                    }
                }
            }
            Ok(json_err(StatusCode::NOT_FOUND, "transaction not found"))
        }

        // GET /api/receipt/:hash
        (Method::GET, ["api", "receipt", tx_hash]) => {
            let target = match hex::decode(tx_hash) {
                Ok(h) => h,
                Err(_) => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid tx hash")),
            };
            let chain = chain.lock().await;
            match chain.get_receipt(&target) {
                Some(receipt) => Ok(json_ok(indexed_receipt_to_api(receipt))),
                None => Ok(json_err(StatusCode::NOT_FOUND, "receipt not found")),
            }
        }

        // GET /api/logs?contract=&topic=&from_block=&to_block=&limit=
        (Method::GET, ["api", "logs"]) => {
            let query = req.uri().query().unwrap_or("");
            let params: Vec<(&str, &str)> =
                query.split('&').filter_map(|p| p.split_once('=')).collect();
            let contract = params
                .iter()
                .find(|(k, _)| *k == "contract")
                .and_then(|(_, v)| hex::decode(v).ok());
            let topic = params
                .iter()
                .find(|(k, _)| *k == "topic")
                .and_then(|(_, v)| hex::decode(v).ok());
            let from_block = params
                .iter()
                .find(|(k, _)| *k == "from_block")
                .and_then(|(_, v)| v.parse().ok());
            let to_block = params
                .iter()
                .find(|(k, _)| *k == "to_block")
                .and_then(|(_, v)| v.parse().ok());
            let limit = params
                .iter()
                .find(|(k, _)| *k == "limit")
                .and_then(|(_, v)| v.parse().ok());
            let chain = chain.lock().await;
            let filter = LogFilter {
                contract,
                topic,
                from_block,
                to_block,
                limit,
            };
            let entries: Vec<ApiLogEntry> = chain
                .query_logs(&filter)
                .into_iter()
                .map(indexed_log_to_api)
                .collect();
            Ok(json_ok(entries))
        }

        // GET /api/pending
        (Method::GET, ["api", "pending"]) => {
            let chain = chain.lock().await;
            let txs: Vec<ApiTransaction> =
                chain.pending_transactions.iter().map(tx_to_api).collect();
            Ok(json_ok(txs))
        }

        // GET /api/validators
        (Method::GET, ["api", "validators"]) => {
            let chain = chain.lock().await;
            let pos = crate::consensus::ProofOfStake::with_slashed(
                chain.minimum_stake,
                chain.slashed_validators.clone(),
                chain.height() + 1,
            );
            let validators: Vec<ApiValidator> = pos
                .active_validators(&chain.accounts)
                .into_iter()
                .map(|v| ApiValidator {
                    address: hex::encode(&v.address),
                    public_key: hex::encode(&v.public_key),
                    stake: v.stake,
                })
                .collect();
            Ok(json_ok(validators))
        }

        // GET /api/faucet/:address
        (Method::GET, ["api", "faucet", addr_hex]) => {
            let addr_clean = addr_hex.strip_prefix("CUR").unwrap_or(addr_hex);
            let address = match hex::decode(addr_clean) {
                Ok(a) if a.len() == hash::ADDRESS_LEN => a,
                _ => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid address")),
            };

            // Check faucet cooldown
            {
                let cooldowns = faucet_cooldowns.lock().await;
                if let Some(last_request) = cooldowns.get(&address) {
                    let elapsed = last_request.elapsed().as_secs();
                    if elapsed < FAUCET_COOLDOWN_SECS {
                        let remaining = FAUCET_COOLDOWN_SECS - elapsed;
                        return Ok(json_err(
                            StatusCode::TOO_MANY_REQUESTS,
                            &format!("faucet cooldown: try again in {} seconds", remaining),
                        ));
                    }
                }
            }

            // Credit the account
            let new_balance = {
                let mut chain = chain.lock().await;
                let account = chain.accounts.entry(address.clone()).or_default();
                account.balance = account.balance.saturating_add(FAUCET_AMOUNT);
                account.balance
            };

            // Record cooldown
            {
                let mut cooldowns = faucet_cooldowns.lock().await;
                cooldowns.insert(address.clone(), Instant::now());
            }

            Ok(json_ok(serde_json::json!({
                "address": hex::encode(&address),
                "amount": FAUCET_AMOUNT,
                "new_balance": new_balance,
                "next_available_secs": FAUCET_COOLDOWN_SECS
            })))
        }

        // POST /api/tx/submit
        (Method::POST, ["api", "tx", "submit"]) => {
            if let Some(response) = enforce_api_auth(&req) {
                return Ok(response);
            }

            if let Some(content_length) = req
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<usize>().ok())
                && content_length > MAX_API_BODY_BYTES
            {
                return Ok(json_err(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body too large",
                ));
            }

            let body_bytes = match http_body_util::BodyExt::collect(req.into_body()).await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => return Ok(json_err(StatusCode::BAD_REQUEST, "failed to read body")),
            };
            if body_bytes.len() > MAX_API_BODY_BYTES {
                return Ok(json_err(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body too large",
                ));
            }

            let tx: Transaction = match serde_json::from_slice(&body_bytes) {
                Ok(t) => t,
                Err(e) => {
                    return Ok(json_err(
                        StatusCode::BAD_REQUEST,
                        &format!("invalid transaction JSON: {}", e),
                    ));
                }
            };

            let tx_hash = tx.hash_hex();
            let mut chain = chain.lock().await;
            match chain.add_transaction(tx) {
                Ok(()) => {
                    if let Some(pending) = chain.pending_transactions.last()
                        && let Ok(data) = bincode::serialize(pending)
                    {
                        let _ = outbound_tx.try_send(NetworkMessage::NewTransaction(data));
                    }
                    let event = serde_json::json!({"type": "new_tx", "hash": tx_hash}).to_string();
                    let _ = event_tx.send(event);
                    Ok(json_ok(serde_json::json!({"tx_hash": tx_hash})))
                }
                Err(e) => Ok(json_err(StatusCode::BAD_REQUEST, &e.to_string())),
            }
        }

        // POST /api/tx/estimate
        (Method::POST, ["api", "tx", "estimate"]) => {
            if let Some(content_length) = req
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<usize>().ok())
                && content_length > MAX_API_BODY_BYTES
            {
                return Ok(json_err(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body too large",
                ));
            }

            let body_bytes = match http_body_util::BodyExt::collect(req.into_body()).await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => return Ok(json_err(StatusCode::BAD_REQUEST, "failed to read body")),
            };
            if body_bytes.len() > MAX_API_BODY_BYTES {
                return Ok(json_err(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body too large",
                ));
            }

            let tx: Transaction = match serde_json::from_slice(&body_bytes) {
                Ok(t) => t,
                Err(e) => {
                    return Ok(json_err(
                        StatusCode::BAD_REQUEST,
                        &format!("invalid transaction JSON: {}", e),
                    ));
                }
            };

            let chain = chain.lock().await;
            match chain.estimate_transaction(&tx) {
                Ok(estimate) => Ok(json_ok(estimate)),
                Err(e) => Ok(json_err(StatusCode::BAD_REQUEST, &e.to_string())),
            }
        }

        _ => Ok(json_err(StatusCode::NOT_FOUND, "endpoint not found")),
    }
}

// ─── HTTP Server ─────────────────────────────────────────────────────

pub async fn serve_http(
    addr: &str,
    chain: Arc<Mutex<Blockchain>>,
    event_tx: broadcast::Sender<String>,
    outbound_tx: mpsc::Sender<NetworkMessage>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = API_START_TIME.get_or_init(Instant::now);
    let listener = TcpListener::bind(addr).await?;
    let connection_limit = Arc::new(Semaphore::new(MAX_HTTP_CONNECTIONS));
    let rate_limiter: RateLimiterMap = Arc::new(Mutex::new(HashMap::new()));
    let request_counter = Arc::new(AtomicU64::new(0));
    let faucet_cooldowns: FaucetCooldownMap = Arc::new(Mutex::new(HashMap::new()));
    tracing::info!("HTTP API listening on http://{}", addr);

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let peer_ip = peer_addr.ip();
        let io = TokioIo::new(stream);
        let chain = Arc::clone(&chain);
        let event_tx = event_tx.clone();
        let outbound_tx = outbound_tx.clone();
        let connection_limit = Arc::clone(&connection_limit);
        let rate_limiter = Arc::clone(&rate_limiter);
        let request_counter = Arc::clone(&request_counter);
        let faucet_cooldowns = Arc::clone(&faucet_cooldowns);

        tokio::spawn(async move {
            let Ok(_permit) = connection_limit.acquire_owned().await else {
                return;
            };
            let service = service_fn(move |req| {
                let chain = Arc::clone(&chain);
                let event_tx = event_tx.clone();
                let outbound_tx = outbound_tx.clone();
                let ctx = RequestContext {
                    peer_ip,
                    rate_limiter: Arc::clone(&rate_limiter),
                    request_counter: Arc::clone(&request_counter),
                    faucet_cooldowns: Arc::clone(&faucet_cooldowns),
                };
                async move { handle_request(req, chain, event_tx, outbound_tx, ctx).await }
            });

            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                tracing::warn!("HTTP connection error: {}", err);
            }
        });
    }
}
