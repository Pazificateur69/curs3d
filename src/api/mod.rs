use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore, broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::core::block::Block;
use crate::core::chain::Blockchain;
use crate::core::receipt::{IndexedLogEntry, IndexedReceipt, LogFilter};
use crate::core::state_proof::{AccountProof, StorageProof};
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::hash;
use crate::network::NetworkMessage;
use crate::wallet;

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
static RATE_LIMIT_REMAINING: AtomicU64 = AtomicU64::new(60);
static RATE_LIMIT_MAX: AtomicU64 = AtomicU64::new(60);

type RateLimiterMap = Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>;
type FaucetCooldownMap = Arc<Mutex<HashMap<String, u64>>>;

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
            TransactionKind::DeployToken => "deploy_token".to_string(),
            TransactionKind::TokenTransfer => "token_transfer".to_string(),
            TransactionKind::TokenApprove => "token_approve".to_string(),
            TransactionKind::TokenTransferFrom => "token_transfer_from".to_string(),
            TransactionKind::SubmitProposal => "submit_proposal".to_string(),
            TransactionKind::GovernanceVote => "governance_vote".to_string(),
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

fn faucet_password() -> Option<String> {
    if let Ok(path) = std::env::var("CURS3D_FAUCET_PASSWORD_FILE")
        && !path.trim().is_empty()
    {
        return std::fs::read_to_string(path)
            .ok()
            .map(|secret| secret.trim().to_string())
            .filter(|secret| !secret.is_empty());
    }

    std::env::var("CURS3D_FAUCET_PASSWORD")
        .ok()
        .filter(|value| !value.is_empty())
}

fn faucet_wallet_configured() -> Option<(String, String, u64)> {
    let wallet_path = std::env::var("CURS3D_FAUCET_WALLET")
        .ok()
        .filter(|value| !value.is_empty())?;
    let password = faucet_password()?;
    let fee = std::env::var("CURS3D_FAUCET_FEE")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1_000);
    Some((wallet_path, password, fee))
}

fn faucet_cooldown_store_path() -> String {
    std::env::var("CURS3D_FAUCET_COOLDOWN_FILE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "faucet_cooldowns.json".to_string())
}

fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn load_faucet_cooldowns() -> HashMap<String, u64> {
    let path = faucet_cooldown_store_path();
    let Ok(data) = std::fs::read(path) else {
        return HashMap::new();
    };
    serde_json::from_slice(&data).unwrap_or_default()
}

fn persist_faucet_cooldowns(cooldowns: &HashMap<String, u64>) {
    let path = faucet_cooldown_store_path();
    if let Ok(data) = serde_json::to_vec_pretty(cooldowns) {
        let _ = std::fs::write(path, data);
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

    let remaining = max_requests.saturating_sub(timestamps.len());

    if timestamps.len() >= max_requests {
        let mut response = json_err(
            StatusCode::TOO_MANY_REQUESTS,
            &format!(
                "rate limit exceeded: max {} requests per minute",
                max_requests
            ),
        );
        let headers = response.headers_mut();
        headers.insert("X-RateLimit-Limit", max_requests.into());
        headers.insert("X-RateLimit-Remaining", 0u64.into());
        headers.insert("X-RateLimit-Window", RATE_LIMIT_WINDOW_SECS.into());
        return Some(response);
    }

    timestamps.push(now);
    // Note: rate-limit headers are added per-request in handle_request wrapper
    RATE_LIMIT_REMAINING.store(remaining.saturating_sub(1) as u64, Ordering::Relaxed);
    RATE_LIMIT_MAX.store(max_requests as u64, Ordering::Relaxed);
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

    // Capture rate-limit info for response headers
    let rl_remaining = RATE_LIMIT_REMAINING.load(Ordering::Relaxed);
    let rl_max = RATE_LIMIT_MAX.load(Ordering::Relaxed);

    let faucet_cooldowns = ctx.faucet_cooldowns;

    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let mut result: Result<Response<Full<Bytes>>, hyper::Error> =
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

            // POST /api/faucet/request
            (Method::POST, ["api", "faucet", "request"]) => {
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

                let payload: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                    Ok(value) => value,
                    Err(_) => {
                        return Ok(json_err(
                            StatusCode::BAD_REQUEST,
                            "invalid faucet request JSON",
                        ));
                    }
                };
                let Some(address_str) = payload.get("address").and_then(|value| value.as_str())
                else {
                    return Ok(json_err(
                        StatusCode::BAD_REQUEST,
                        "missing faucet request address",
                    ));
                };
                let addr_clean = address_str.strip_prefix("CUR").unwrap_or(address_str);
                let address = match hex::decode(addr_clean) {
                    Ok(a) if a.len() == hash::ADDRESS_LEN => a,
                    _ => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid address")),
                };
                let address_key = hex::encode(&address);
                let Some((wallet_path, password, fee)) = faucet_wallet_configured() else {
                    return Ok(json_err(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "faucet disabled: configure CURS3D_FAUCET_WALLET and password",
                    ));
                };
                let faucet_wallet = match wallet::Wallet::load_auto(&wallet_path, &password) {
                    Ok(wallet) => wallet,
                    Err(_) => {
                        return Ok(json_err(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "faucet unavailable: failed to load configured faucet wallet",
                        ));
                    }
                };
                let faucet_address =
                    hash::address_bytes_from_public_key(&faucet_wallet.keypair.public_key);

                // Check faucet cooldown
                {
                    let cooldowns = faucet_cooldowns.lock().await;
                    if let Some(last_request) = cooldowns.get(&address_key) {
                        let elapsed = current_unix_timestamp().saturating_sub(*last_request);
                        if elapsed < FAUCET_COOLDOWN_SECS {
                            let remaining = FAUCET_COOLDOWN_SECS - elapsed;
                            return Ok(json_err(
                                StatusCode::TOO_MANY_REQUESTS,
                                &format!("faucet cooldown: try again in {} seconds", remaining),
                            ));
                        }
                    }
                }

                let tx = {
                    let mut chain = chain.lock().await;
                    let faucet_account = chain.get_account(&faucet_address);
                    let total_needed = FAUCET_AMOUNT.saturating_add(fee);
                    if faucet_account.balance < total_needed {
                        return Ok(json_err(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "faucet depleted: refill the configured faucet wallet",
                        ));
                    }

                    let mut tx = Transaction::new(
                        chain.chain_id(),
                        faucet_wallet.keypair.public_key.clone(),
                        address.clone(),
                        FAUCET_AMOUNT,
                        fee,
                        faucet_account.nonce,
                    );
                    tx.sign(&faucet_wallet.keypair);
                    if let Err(err) = chain.add_transaction(tx.clone()) {
                        return Ok(json_err(
                            StatusCode::BAD_REQUEST,
                            &format!("faucet transfer rejected: {}", err),
                        ));
                    }
                    tx
                };

                if let Ok(data) = bincode::serialize(&tx) {
                    let _ = outbound_tx.try_send(NetworkMessage::NewTransaction(data));
                };

                // Record cooldown
                {
                    let mut cooldowns = faucet_cooldowns.lock().await;
                    cooldowns.insert(address_key, current_unix_timestamp());
                    persist_faucet_cooldowns(&cooldowns);
                }

                Ok(json_ok(serde_json::json!({
                    "address": hex::encode(&address),
                    "amount": FAUCET_AMOUNT,
                    "tx_hash": tx.hash_hex(),
                    "from": faucet_wallet.address,
                    "fee": fee,
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
                        let event =
                        serde_json::json!({"type": "new_transaction", "data": {"hash": tx_hash}})
                            .to_string();
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

            // ─── CUR-20 Token Endpoints ─────────────────────────────────

            // GET /api/tokens — list all tokens
            (Method::GET, ["api", "tokens"]) => {
                let chain = chain.lock().await;
                let tokens: Vec<serde_json::Value> = chain
                    .token_registry
                    .list_tokens()
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "address": format!("CUR{}", hex::encode(&t.contract_address)),
                            "name": t.name,
                            "symbol": t.symbol,
                            "decimals": t.decimals,
                            "total_supply": t.total_supply,
                            "creator": format!("CUR{}", hex::encode(&t.creator)),
                            "created_at_height": t.created_at_height,
                        })
                    })
                    .collect();
                Ok(json_ok(tokens))
            }

            // GET /api/token/<address> — token info
            (Method::GET, ["api", "token", address]) => {
                let addr = match parse_address(address) {
                    Some(a) => a,
                    None => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid token address")),
                };
                let chain = chain.lock().await;
                match chain.token_registry.get_token(&addr) {
                    Some(token) => Ok(json_ok(serde_json::json!({
                        "address": format!("CUR{}", hex::encode(&token.contract_address)),
                        "name": token.name,
                        "symbol": token.symbol,
                        "decimals": token.decimals,
                        "total_supply": token.total_supply,
                        "creator": format!("CUR{}", hex::encode(&token.creator)),
                        "created_at_height": token.created_at_height,
                    }))),
                    None => Ok(json_err(StatusCode::NOT_FOUND, "token not found")),
                }
            }

            // GET /api/token/<address>/balance/<owner> — token balance
            (Method::GET, ["api", "token", token_addr, "balance", owner_addr]) => {
                let token = match parse_address(token_addr) {
                    Some(a) => a,
                    None => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid token address")),
                };
                let owner = match parse_address(owner_addr) {
                    Some(a) => a,
                    None => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid owner address")),
                };
                let chain = chain.lock().await;
                let balance = chain.token_registry.balance_of(&token, &owner);
                Ok(json_ok(serde_json::json!({ "balance": balance })))
            }

            // ─── Governance Endpoints ───────────────────────────────────

            // GET /api/governance/proposals — list all proposals
            (Method::GET, ["api", "governance", "proposals"]) => {
                let chain = chain.lock().await;
                let proposals: Vec<serde_json::Value> = chain
                    .governance
                    .list_proposals()
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "id": hex::encode(&p.id),
                            "proposer": format!("CUR{}", hex::encode(&p.proposer)),
                            "kind": format!("{:?}", p.kind),
                            "status": format!("{:?}", p.status),
                            "created_at_height": p.created_at_height,
                            "voting_deadline_height": p.voting_deadline_height,
                            "execution_height": p.execution_height,
                            "votes_for": p.votes_for,
                            "votes_against": p.votes_against,
                            "voter_count": p.voters.len(),
                        })
                    })
                    .collect();
                Ok(json_ok(proposals))
            }

            // GET /api/governance/proposal/<id> — proposal details
            (Method::GET, ["api", "governance", "proposal", id_hex]) => {
                let id = match hex::decode(id_hex) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return Ok(json_err(StatusCode::BAD_REQUEST, "invalid proposal id hex"));
                    }
                };
                let chain = chain.lock().await;
                match chain.governance.get_proposal(&id) {
                    Some(p) => Ok(json_ok(serde_json::json!({
                        "id": hex::encode(&p.id),
                        "proposer": format!("CUR{}", hex::encode(&p.proposer)),
                        "kind": format!("{:?}", p.kind),
                        "status": format!("{:?}", p.status),
                        "created_at_height": p.created_at_height,
                        "voting_deadline_height": p.voting_deadline_height,
                        "execution_height": p.execution_height,
                        "votes_for": p.votes_for,
                        "votes_against": p.votes_against,
                        "voter_count": p.voters.len(),
                    }))),
                    None => Ok(json_err(StatusCode::NOT_FOUND, "proposal not found")),
                }
            }

            _ => Ok(json_err(StatusCode::NOT_FOUND, "endpoint not found")),
        };

    // Inject rate-limit headers into every response
    if let Ok(ref mut response) = result {
        let headers = response.headers_mut();
        headers.insert("X-RateLimit-Limit", rl_max.into());
        headers.insert("X-RateLimit-Remaining", rl_remaining.into());
        headers.insert("X-RateLimit-Window", RATE_LIMIT_WINDOW_SECS.into());
    }

    result
}

// ─── WebSocket Support ──────────────────────────────────────────────

const WS_MAX_CONNECTIONS: usize = 64;

#[derive(Debug, Deserialize)]
struct WsSubscribeRequest {
    #[serde(default)]
    events: Vec<String>,
}

fn parse_address(addr_hex: &str) -> Option<Vec<u8>> {
    let clean = addr_hex.strip_prefix("CUR").unwrap_or(addr_hex);
    hex::decode(clean)
        .ok()
        .filter(|a| a.len() == hash::ADDRESS_LEN)
}

fn is_websocket_upgrade(buf: &[u8]) -> bool {
    if let Ok(text) = std::str::from_utf8(buf) {
        let lower = text.to_ascii_lowercase();
        lower.contains("upgrade: websocket") || lower.contains("get /ws")
    } else {
        false
    }
}

async fn handle_ws_connection(stream: TcpStream, mut event_rx: broadcast::Receiver<String>) {
    let ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            tracing::warn!("WebSocket handshake failed: {}", e);
            return;
        }
    };

    tracing::info!("WebSocket client connected");

    let (mut ws_tx, mut ws_rx) = ws.split();
    let mut subscribed_events: HashSet<String> = HashSet::new();
    // Subscribe to all events by default
    subscribed_events.insert("new_block".to_string());
    subscribed_events.insert("new_transaction".to_string());
    subscribed_events.insert("finality".to_string());

    loop {
        tokio::select! {
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        // Parse subscription request
                        if let Ok(sub) = serde_json::from_str::<WsSubscribeRequest>(&text) {
                            subscribed_events.clear();
                            for event in sub.events {
                                subscribed_events.insert(event);
                            }
                            let ack = serde_json::json!({
                                "type": "subscribed",
                                "data": { "events": subscribed_events.iter().collect::<Vec<_>>() }
                            });
                            if ws_tx.send(WsMessage::Text(ack.to_string())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(WsMessage::Ping(data))) => {
                        if ws_tx.send(WsMessage::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
            event = event_rx.recv() => {
                match event {
                    Ok(event_str) => {
                        // Parse event type and check subscription
                        if let Ok(event_json) = serde_json::from_str::<serde_json::Value>(&event_str) {
                            let event_type = event_json
                                .get("type")
                                .and_then(|t| t.as_str())
                                .unwrap_or("");

                            if (subscribed_events.contains(event_type) || subscribed_events.is_empty())
                                && ws_tx.send(WsMessage::Text(event_str)).await.is_err()
                            {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket client lagged by {} events", n);
                    }
                    Err(_) => break,
                }
            }
        }
    }

    tracing::info!("WebSocket client disconnected");
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
    let ws_connection_count = Arc::new(AtomicU64::new(0));
    let rate_limiter: RateLimiterMap = Arc::new(Mutex::new(HashMap::new()));
    let request_counter = Arc::new(AtomicU64::new(0));
    let faucet_cooldowns: FaucetCooldownMap = Arc::new(Mutex::new(load_faucet_cooldowns()));
    tracing::info!("HTTP API listening on http://{}", addr);
    tracing::info!("WebSocket available at ws://{}/ws", addr);

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let peer_ip = peer_addr.ip();

        // Peek at the first bytes to detect WebSocket upgrade
        let mut peek_buf = [0u8; 512];
        let n = match stream.peek(&mut peek_buf).await {
            Ok(n) => n,
            Err(_) => continue,
        };

        if is_websocket_upgrade(&peek_buf[..n]) {
            // WebSocket connection
            let ws_count = Arc::clone(&ws_connection_count);
            let current = ws_count.load(Ordering::Relaxed);
            if current >= WS_MAX_CONNECTIONS as u64 {
                tracing::warn!("WebSocket connection limit reached, rejecting {}", peer_ip);
                continue;
            }
            ws_count.fetch_add(1, Ordering::Relaxed);

            let event_rx = event_tx.subscribe();
            tokio::spawn(async move {
                handle_ws_connection(stream, event_rx).await;
                ws_count.fetch_sub(1, Ordering::Relaxed);
            });
            continue;
        }

        // Regular HTTP connection
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
