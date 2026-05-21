pub mod eth_rpc;

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
use crate::light::SignedHeader;
use crate::network::NetworkMessage;
use crate::runtime::{NodeRole, SharedRuntimeState};
use crate::wallet;

const MAX_API_BODY_BYTES: usize = 1024 * 1024;
const MAX_HTTP_CONNECTIONS: usize = 128;
const RATE_LIMIT_GET: usize = 60;
const RATE_LIMIT_POST: usize = 10;
/// Higher cap for the Ethereum-compatible JSON-RPC endpoint (`/eth`). MetaMask,
/// ethers.js, viem, hardhat, foundry — they all poll dozens of methods per
/// block (eth_blockNumber, eth_getBlockByNumber, eth_getTransactionCount,
/// eth_estimateGas, etc.). 60/min strangles real EVM tooling. 600/min = 10/sec
/// per-IP is comfortable for a single dApp user and still bounds abuse.
const RATE_LIMIT_ETH: usize = 600;
const RATE_LIMIT_WINDOW_SECS: u64 = 60;
const RATE_LIMIT_CLEANUP_SECS: u64 = 120;
const RATE_LIMIT_CLEANUP_INTERVAL: u64 = 100;
const FAUCET_AMOUNT: u64 = 100_000_000;
const FAUCET_COOLDOWN_SECS: u64 = 3600;
const MAX_HEALTHY_BLOCK_AGE_SECS: i64 = 120;
/// Liveness check: head must not outrun finality by more than this many
/// blocks. At 10s slots, 50 blocks ≈ 8 minutes of unfinalised tip — past
/// that the BFT consensus is effectively broken even if blocks keep coming.
const MAX_FINALITY_LAG_BLOCKS: u64 = 50;
static API_START_TIME: OnceLock<Instant> = OnceLock::new();
static RATE_LIMIT_REMAINING: AtomicU64 = AtomicU64::new(60);
static RATE_LIMIT_MAX: AtomicU64 = AtomicU64::new(60);

type RateLimiterMap = Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>;
type FaucetCooldownMap = Arc<Mutex<HashMap<String, u64>>>;
type FaucetIpCooldownMap = Arc<Mutex<HashMap<String, u64>>>;

struct RequestContext {
    peer_ip: IpAddr,
    rate_limiter: RateLimiterMap,
    request_counter: Arc<AtomicU64>,
    faucet_cooldowns: FaucetCooldownMap,
    faucet_ip_cooldowns: FaucetIpCooldownMap,
    runtime_state: SharedRuntimeState,
}

// ─── API Response Types ──────────────────────────────────────────────

#[derive(Serialize)]
struct ApiResponse<T: Serialize> {
    ok: bool,
    data: Option<T>,
    error: Option<String>,
}

fn json_ok<T: Serialize>(data: T) -> Response<Full<Bytes>> {
    json_ok_with_origin(data, false)
}

fn finish_response(
    builder: hyper::http::response::Builder,
    body: impl Into<Bytes>,
) -> Response<Full<Bytes>> {
    builder.body(Full::new(body.into())).unwrap_or_else(|err| {
        tracing::error!(error = %err, "failed to build HTTP response");
        Response::new(Full::new(Bytes::from_static(
            br#"{"ok":false,"data":null,"error":"internal response error"}"#,
        )))
    })
}

fn json_response<T: Serialize>(status: StatusCode, data: T) -> Response<Full<Bytes>> {
    let resp = ApiResponse {
        ok: status.is_success(),
        data: Some(data),
        error: None,
    };
    let body = serde_json::to_string(&resp).unwrap_or_default();
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", "application/json");
    builder = with_cors_headers(builder);
    finish_response(builder, body)
}

fn json_ok_with_origin<T: Serialize>(data: T, force_public: bool) -> Response<Full<Bytes>> {
    let resp = ApiResponse {
        ok: true,
        data: Some(data),
        error: None,
    };
    let body = serde_json::to_string(&resp).unwrap_or_default();
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json");
    builder = if force_public {
        with_public_cors_headers(builder)
    } else {
        with_cors_headers(builder)
    };
    finish_response(builder, body)
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
    finish_response(builder, body)
}

fn text_response(status: StatusCode, content_type: &str, body: String) -> Response<Full<Bytes>> {
    let mut builder = Response::builder()
        .status(status)
        .header("Content-Type", content_type);
    builder = with_cors_headers(builder);
    finish_response(builder, body)
}

fn cors_preflight() -> Response<Full<Bytes>> {
    if cors_allow_origin().is_none() {
        return finish_response(
            Response::builder().status(StatusCode::FORBIDDEN),
            Bytes::new(),
        );
    }
    let mut builder = Response::builder().status(StatusCode::NO_CONTENT);
    builder = with_cors_headers(builder);
    finish_response(builder, Bytes::new())
}

/// Preflight reply for the always-public ETH JSON-RPC endpoint. Used regardless
/// of `CURS3D_API_ALLOW_ORIGIN` so dApps and Metamask can reach the node from
/// any origin.
fn cors_preflight_public() -> Response<Full<Bytes>> {
    let mut builder = Response::builder().status(StatusCode::NO_CONTENT);
    builder = with_public_cors_headers(builder);
    finish_response(builder, Bytes::new())
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

/// Permissive CORS headers for endpoints that must be reachable from any origin
/// (the Ethereum-compatible JSON-RPC: Metamask, ethers.js, wagmi).
fn with_public_cors_headers(
    builder: hyper::http::response::Builder,
) -> hyper::http::response::Builder {
    builder
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header(
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization",
        )
        .header("Access-Control-Max-Age", "600")
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
    latest_block_age_secs: i64,
    pending_transactions: usize,
    active_validators: usize,
    peer_count: usize,
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
    node_role: NodeRole,
    validator_address: Option<String>,
    network_online: bool,
    peer_count: usize,
    bootnode_count: usize,
    rpc_addr: String,
    http_addr: String,
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
            TransactionKind::DeployEvmContract => "deploy_evm_contract".to_string(),
            TransactionKind::CallEvmContract => "call_evm_contract".to_string(),
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

fn faucet_ip_cooldown_store_path() -> String {
    std::env::var("CURS3D_FAUCET_IP_COOLDOWN_FILE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "faucet_ip_cooldowns.json".to_string())
}

fn faucet_ip_cooldown_secs() -> u64 {
    std::env::var("CURS3D_FAUCET_IP_COOLDOWN_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(FAUCET_COOLDOWN_SECS)
}

fn load_faucet_ip_cooldowns() -> HashMap<String, u64> {
    let path = faucet_ip_cooldown_store_path();
    let Ok(data) = std::fs::read(path) else {
        return HashMap::new();
    };
    serde_json::from_slice(&data).unwrap_or_default()
}

fn persist_faucet_ip_cooldowns(cooldowns: &HashMap<String, u64>) {
    let path = faucet_ip_cooldown_store_path();
    if let Ok(data) = serde_json::to_vec_pretty(cooldowns) {
        let _ = std::fs::write(path, data);
    }
}

fn faucet_require_captcha() -> bool {
    std::env::var("CURS3D_FAUCET_REQUIRE_CAPTCHA")
        .ok()
        .filter(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .is_some()
}

fn faucet_captcha_secret() -> Option<String> {
    std::env::var("CURS3D_FAUCET_CAPTCHA_SECRET")
        .ok()
        .filter(|value| !value.is_empty())
}

/// Constant-time byte comparison to prevent timing attacks on the shared secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Captcha verification is delegated to the reverse proxy (nginx auth_request
/// against hCaptcha or Cloudflare Turnstile). The proxy verifies the captcha
/// then forwards two headers:
///   - `X-Captcha-Verified: 1`
///   - `X-Captcha-Secret: <shared-secret>` (matches CURS3D_FAUCET_CAPTCHA_SECRET)
///
/// Without the shared secret, a client could simply curl the node with
/// `X-Captcha-Verified: 1` and bypass the gate. The shared secret guarantees
/// the request actually flowed through the configured proxy.
///
/// If `CURS3D_FAUCET_CAPTCHA_SECRET` is not set, the node logs a warning and
/// rejects all requests when captcha is required (fail-closed).
fn captcha_verified(req: &Request<Incoming>) -> bool {
    let verified_header = req
        .headers()
        .get("x-captcha-verified")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if verified_header != "1" {
        return false;
    }

    let Some(expected_secret) = faucet_captcha_secret() else {
        tracing::warn!(
            "captcha required but CURS3D_FAUCET_CAPTCHA_SECRET is not set — rejecting request"
        );
        return false;
    };

    let Some(provided_secret) = req
        .headers()
        .get("x-captcha-secret")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    constant_time_eq(expected_secret.as_bytes(), provided_secret.as_bytes())
}

// ─── Request Router ──────────────────────────────────────────────────

async fn check_rate_limit(
    peer_ip: IpAddr,
    method: &Method,
    path: &str,
    rate_limiter: &RateLimiterMap,
    request_counter: &AtomicU64,
) -> Option<Response<Full<Bytes>>> {
    let now = Instant::now();
    // /eth is a JSON-RPC endpoint dominated by read methods. MetaMask polls
    // eth_blockNumber every block; foundry/forge issue dozens of calls per
    // contract deploy. The previous 60/min cap throttled real EVM tooling.
    // Use a dedicated higher bucket (600/min = 10/sec) for /eth.
    let is_eth_rpc = path == "/eth" || path == "/rpc/eth";
    let max_requests = if is_eth_rpc {
        RATE_LIMIT_ETH
    } else if *method == Method::POST {
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
    let req_path = req.uri().path().to_string();
    let is_eth_rpc = req_path == "/eth" || req_path == "/rpc/eth";

    if req.method() == Method::OPTIONS {
        if is_eth_rpc {
            return Ok(cors_preflight_public());
        }
        return Ok(cors_preflight());
    }

    // Rate limit check before any processing
    if let Some(response) = check_rate_limit(
        ctx.peer_ip,
        req.method(),
        &req_path,
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
    let faucet_ip_cooldowns = ctx.faucet_ip_cooldowns;
    // Behind a reverse proxy (nginx on 127.0.0.1 / ::1), the TCP peer is
    // always loopback; the real client IP lives in X-Real-IP (preferred,
    // set by the operator's nginx) or X-Forwarded-For (right-most non-trusted
    // entry). Trust those headers ONLY when the TCP peer is loopback —
    // otherwise an external attacker could spoof their IP for rate-limit /
    // faucet-cooldown bypass.
    let peer_ip = if ctx.peer_ip.is_loopback() {
        let from_real_ip = req
            .headers()
            .get("x-real-ip")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.trim().parse::<IpAddr>().ok());
        let from_xff = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse::<IpAddr>().ok());
        from_real_ip.or(from_xff).unwrap_or(ctx.peer_ip)
    } else {
        ctx.peer_ip
    };
    let runtime_state = Arc::clone(&ctx.runtime_state);

    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let mut result: Result<Response<Full<Bytes>>, hyper::Error> = match (
        method,
        segments.as_slice(),
    ) {
        // GET /api/healthz
        (Method::GET, ["api", "healthz"]) => {
            let chain = chain.lock().await;
            let latest_ts = chain.latest_block().header.timestamp;
            let age = chrono::Utc::now().timestamp().saturating_sub(latest_ts);
            let runtime = runtime_state.read().await.snapshot();
            // Liveness: producing/syncing recent blocks AND finality is not far
            // behind. A BFT chain that produces but never finalises is broken
            // even if blocks keep coming. We accept up to MAX_FINALITY_LAG_BLOCKS
            // (50) of head→finalized lag — that's ~8 minutes at 10 s slots.
            let head = chain.height();
            let finalized = chain.finalized_height();
            let finality_lag = head.saturating_sub(finalized);
            let healthy = runtime.network_online
                && age <= MAX_HEALTHY_BLOCK_AGE_SECS
                && finality_lag <= MAX_FINALITY_LAG_BLOCKS;
            Ok(json_response(
                if healthy {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                ApiHealth {
                    ok: healthy,
                    chain_id: chain.chain_id().to_string(),
                    height: chain.height(),
                    finalized_height: chain.finalized_height(),
                    latest_block_timestamp: latest_ts,
                    latest_block_age_secs: age,
                    pending_transactions: chain.pending_transactions.len(),
                    node_role: runtime.role,
                    validator_address: runtime.validator_address,
                    network_online: runtime.network_online,
                    peer_count: runtime.peer_count,
                    bootnode_count: runtime.bootnode_count,
                    rpc_addr: runtime.rpc_addr,
                    http_addr: runtime.http_addr,
                },
            ))
        }

        // GET /api/metrics
        (Method::GET, ["api", "metrics"]) => {
            let chain = chain.lock().await;
            let runtime = runtime_state.read().await.snapshot();
            let uptime = API_START_TIME
                .get()
                .map(|start| start.elapsed().as_secs())
                .unwrap_or_default();
            let latest_ts = chain.latest_block().header.timestamp;
            let block_age = chrono::Utc::now()
                .timestamp()
                .saturating_sub(latest_ts)
                .max(0) as u64;
            let proto_version = chain.protocol_version_at_height(chain.height());
            let head = chain.height();
            let final_height = chain.finalized_height();
            let finality_lag = head.saturating_sub(final_height);
            let (mempool_sys, mempool_user, mempool_gas_usage, mempool_gas_budget) =
                chain.mempool_stats();
            let slashed = chain.slashed_validator_count();
            let jailed = chain.jailed_validator_count();
            let body = format!(
                concat!(
                    "# TYPE curs3d_uptime_seconds counter\n",
                    "curs3d_uptime_seconds {}\n",
                    "# TYPE curs3d_chain_height gauge\n",
                    "curs3d_chain_height {}\n",
                    "# TYPE curs3d_finalized_height gauge\n",
                    "curs3d_finalized_height {}\n",
                    "# TYPE curs3d_finality_lag gauge\n",
                    "# Head height minus finalized height. Healthy = small (< 5 typically).\n",
                    "curs3d_finality_lag {}\n",
                    "# TYPE curs3d_latest_block_age_seconds gauge\n",
                    "# Wall-clock seconds since the latest block's timestamp. Stall alerts \n",
                    "# fire when this stays above ~30s (chain produces every 10s).\n",
                    "curs3d_latest_block_age_seconds {}\n",
                    "# TYPE curs3d_protocol_version gauge\n",
                    "curs3d_protocol_version {}\n",
                    "# TYPE curs3d_pending_transactions gauge\n",
                    "curs3d_pending_transactions {}\n",
                    "# TYPE curs3d_pending_transactions_system gauge\n",
                    "# Mempool count in MempoolClass::System (Stake/Unstake/governance).\n",
                    "curs3d_pending_transactions_system {}\n",
                    "# TYPE curs3d_pending_transactions_user gauge\n",
                    "# Mempool count in MempoolClass::User (Transfer/CallContract/...).\n",
                    "curs3d_pending_transactions_user {}\n",
                    "# TYPE curs3d_mempool_gas_usage gauge\n",
                    "curs3d_mempool_gas_usage {}\n",
                    "# TYPE curs3d_mempool_gas_budget gauge\n",
                    "curs3d_mempool_gas_budget {}\n",
                    "# TYPE curs3d_active_validators gauge\n",
                    "curs3d_active_validators {}\n",
                    "# TYPE curs3d_slashed_validators_total gauge\n",
                    "# Cumulative validators ever slashed (equivocation, etc.).\n",
                    "curs3d_slashed_validators_total {}\n",
                    "# TYPE curs3d_jailed_validators gauge\n",
                    "# Validators currently jailed (jailed_until_height > head).\n",
                    "curs3d_jailed_validators {}\n",
                    "# TYPE curs3d_peer_count gauge\n",
                    "curs3d_peer_count {}\n",
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
                head,
                final_height,
                finality_lag,
                block_age,
                proto_version,
                chain.pending_transactions.len(),
                mempool_sys,
                mempool_user,
                mempool_gas_usage,
                mempool_gas_budget,
                chain.active_validator_count(),
                slashed,
                jailed,
                runtime.peer_count,
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
            let latest_ts = chain.latest_block().header.timestamp;
            let age = chrono::Utc::now().timestamp().saturating_sub(latest_ts);
            let runtime = runtime_state.read().await.snapshot();
            Ok(json_ok(ApiStatus {
                chain_id: chain.chain_id().to_string(),
                chain_name: chain.genesis_config.chain_name.clone(),
                epoch: chain.current_epoch(),
                epoch_start_height: chain.current_epoch_start_height(),
                height: chain.height(),
                finalized_height: chain.finalized_height(),
                latest_hash: hex::encode(chain.latest_hash()),
                genesis_hash: hex::encode(chain.genesis_hash()),
                latest_block_age_secs: age,
                pending_transactions: chain.pending_transactions.len(),
                active_validators: chain.active_validator_count(),
                peer_count: runtime.peer_count,
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

        // ─── Light Client endpoints ──────────────────────────────────
        // GET /api/genesis — minimal info for a light client to anchor on
        (Method::GET, ["api", "genesis"]) => {
            let chain = chain.lock().await;
            let genesis = chain.blocks.first();
            Ok(json_ok(serde_json::json!({
                "chain_id": chain.chain_id(),
                "chain_name": chain.genesis_config.chain_name,
                "genesis_hash": genesis.map(|b| hex::encode(&b.hash)),
                "genesis_timestamp": genesis.map(|b| b.header.timestamp),
                "genesis_state_root": genesis.map(|b| hex::encode(&b.header.state_root)),
                "block_gas_limit": chain.genesis_config.block_gas_limit,
                "minimum_stake": chain.minimum_stake,
                "epoch_length": chain.genesis_config.epoch_length,
                "protocol_version": genesis.map(|b| b.header.version).unwrap_or(0),
            })))
        }

        // GET /api/headers?from=N&to=M (signed headers for light client sync)
        (Method::GET, ["api", "headers"]) => {
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
                .unwrap_or(64)
                .min(256);
            let to: u64 = params
                .iter()
                .find(|(k, _)| *k == "to")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(from + limit as u64);
            let chain = chain.lock().await;
            let chain_id = chain.chain_id().to_string();
            let height = chain.height();
            let upper = to.min(height).min(from + limit as u64);
            let mut headers: Vec<SignedHeader> = Vec::new();
            for h in from..=upper {
                if let Some(block) = chain.blocks.get(h as usize) {
                    headers.push(SignedHeader {
                        chain_id: chain_id.clone(),
                        header: block.header.clone(),
                        block_hash: block.hash.clone(),
                        signature: block.signature.clone(),
                    });
                }
            }
            Ok(json_ok(serde_json::json!({
                "from": from,
                "to": upper,
                "count": headers.len(),
                "headers": headers,
            })))
        }

        // GET /api/header/:height
        (Method::GET, ["api", "header", height_str]) => {
            let height: u64 = match height_str.parse() {
                Ok(h) => h,
                Err(_) => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid height")),
            };
            let chain = chain.lock().await;
            match chain.blocks.get(height as usize) {
                Some(block) => {
                    let signed = SignedHeader {
                        chain_id: chain.chain_id().to_string(),
                        header: block.header.clone(),
                        block_hash: block.hash.clone(),
                        signature: block.signature.clone(),
                    };
                    Ok(json_ok(signed))
                }
                None => Ok(json_err(StatusCode::NOT_FOUND, "header not found")),
            }
        }

        // GET /api/finality — finalized height + finalized hash for light clients
        (Method::GET, ["api", "finality"]) => {
            let chain = chain.lock().await;
            Ok(json_ok(serde_json::json!({
                "finalized_height": chain.finality_tracker.finalized_height,
                "finalized_hash": hex::encode(&chain.finality_tracker.finalized_hash),
                "current_height": chain.height(),
            })))
        }

        // GET /api/block-by-hash/:hash
        (Method::GET, ["api", "block-by-hash", hash_str]) => {
            let hash_bytes = match hex::decode(hash_str) {
                Ok(b) => b,
                Err(_) => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid hash")),
            };
            let chain = chain.lock().await;
            let height = chain.block_hash_to_height.get(&hash_bytes).copied();
            match height.and_then(|h| chain.blocks.get(h as usize)) {
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

        // GET /api/account/:address/transactions?from_block=&to_block=&limit=
        (Method::GET, ["api", "account", addr_hex, "transactions"]) => {
            let addr_clean = addr_hex.strip_prefix("CUR").unwrap_or(addr_hex);
            let address = match hex::decode(addr_clean) {
                Ok(a) if a.len() == hash::ADDRESS_LEN => a,
                _ => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid address")),
            };
            let query = req.uri().query().unwrap_or("");
            let params: Vec<(&str, &str)> =
                query.split('&').filter_map(|p| p.split_once('=')).collect();
            let from_block = params
                .iter()
                .find(|(k, _)| *k == "from_block")
                .and_then(|(_, v)| v.parse().ok());
            let to_block = params
                .iter()
                .find(|(k, _)| *k == "to_block")
                .and_then(|(_, v)| v.parse().ok());
            let limit: usize = params
                .iter()
                .find(|(k, _)| *k == "limit")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(50)
                .min(500);
            let chain = chain.lock().await;
            #[derive(Serialize)]
            struct ApiAccountTx {
                block_height: u64,
                tx_index: usize,
                #[serde(flatten)]
                tx: ApiTransaction,
            }
            let txs: Vec<ApiAccountTx> = chain
                .transactions_for_address(&address, from_block, to_block, limit)
                .into_iter()
                .map(|(h, i, tx)| ApiAccountTx {
                    block_height: h,
                    tx_index: i,
                    tx: tx_to_api(&tx),
                })
                .collect();
            Ok(json_ok(txs))
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

        // GET /api/contract/:address/code — raw deployed bytecode (hex)
        (Method::GET, ["api", "contract", contract_hex, "code"]) => {
            let contract_address = match hex::decode(contract_hex) {
                Ok(a) if a.len() == hash::ADDRESS_LEN => a,
                _ => {
                    return Ok(json_err(
                        StatusCode::BAD_REQUEST,
                        "invalid contract address",
                    ));
                }
            };
            let chain = chain.lock().await;
            match chain.contracts.get(&contract_address) {
                Some(state) => Ok(json_ok(serde_json::json!({
                    "address": hex::encode(&contract_address),
                    "code": hex::encode(&state.code),
                    "code_length": state.code.len(),
                    "code_hash": hex::encode(hash::sha3_hash(&state.code)),
                    "owner": hex::encode(&state.owner),
                }))),
                None => Ok(json_err(StatusCode::NOT_FOUND, "contract not found")),
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
            if let Some((height, idx)) = chain.tx_hash_index.get(&target).copied()
                && let Some(block) = chain.blocks.get(height as usize)
                && let Some(tx) = block.transactions.get(idx)
            {
                return Ok(json_ok(tx_to_api(tx)));
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
            // Positional topics: topic0, topic1, topic2, topic3 (eth-style)
            let mut positional: Vec<Option<Vec<u8>>> = Vec::new();
            for i in 0..4 {
                let key = format!("topic{}", i);
                let value = params
                    .iter()
                    .find(|(k, _)| *k == key.as_str())
                    .map(|(_, v)| *v);
                if let Some(v) = value {
                    positional.push(hex::decode(v).ok());
                } else {
                    positional.push(None);
                }
            }
            let topics_filter = if positional.iter().any(|p| p.is_some()) {
                Some(positional)
            } else {
                None
            };
            let chain = chain.lock().await;
            let filter = LogFilter {
                contract,
                topic,
                topics: topics_filter,
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
            if faucet_require_captcha() && !captcha_verified(&req) {
                return Ok(json_err(
                    StatusCode::FORBIDDEN,
                    "captcha verification required",
                ));
            }

            // Per-IP cooldown (independent of address), prevents wallet hopping.
            // Note: the actual check+update is performed atomically below
            // (after we know the address is well-formed) to close a race where
            // two requests in the same second could both pass the check.
            let ip_key = peer_ip.to_string();
            let ip_cooldown = faucet_ip_cooldown_secs();

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
            let Some(address_str) = payload.get("address").and_then(|value| value.as_str()) else {
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

            // Atomic cooldown check+reservation. Holding both mutexes across
            // the check AND the optimistic write closes the race where two
            // concurrent requests could both pass the check and then both
            // update the map (#8). On any later failure we roll back.
            let now_ts = current_unix_timestamp();
            let (prev_addr_ts, prev_ip_ts) = {
                let mut addr_cooldowns = faucet_cooldowns.lock().await;
                let mut ip_cooldowns = faucet_ip_cooldowns.lock().await;

                if let Some(last_request) = addr_cooldowns.get(&address_key) {
                    let elapsed = now_ts.saturating_sub(*last_request);
                    if elapsed < FAUCET_COOLDOWN_SECS {
                        let remaining = FAUCET_COOLDOWN_SECS - elapsed;
                        return Ok(json_err(
                            StatusCode::TOO_MANY_REQUESTS,
                            &format!("faucet cooldown: try again in {} seconds", remaining),
                        ));
                    }
                }
                if let Some(last_request) = ip_cooldowns.get(&ip_key) {
                    let elapsed = now_ts.saturating_sub(*last_request);
                    if elapsed < ip_cooldown {
                        let remaining = ip_cooldown - elapsed;
                        return Ok(json_err(
                            StatusCode::TOO_MANY_REQUESTS,
                            &format!("ip cooldown: try again in {} seconds", remaining),
                        ));
                    }
                }

                let prev_addr_ts = addr_cooldowns.insert(address_key.clone(), now_ts);
                let prev_ip_ts = ip_cooldowns.insert(ip_key.clone(), now_ts);
                (prev_addr_ts, prev_ip_ts)
            };

            // Helper for the rollback path so we don't ship a bumped cooldown
            // for a request that ultimately failed.
            let rollback_cooldowns = || async {
                let mut addr_cooldowns = faucet_cooldowns.lock().await;
                match prev_addr_ts {
                    Some(ts) => {
                        addr_cooldowns.insert(address_key.clone(), ts);
                    }
                    None => {
                        addr_cooldowns.remove(&address_key);
                    }
                }
                let mut ip_cooldowns = faucet_ip_cooldowns.lock().await;
                match prev_ip_ts {
                    Some(ts) => {
                        ip_cooldowns.insert(ip_key.clone(), ts);
                    }
                    None => {
                        ip_cooldowns.remove(&ip_key);
                    }
                }
            };

            let tx_or_err = {
                let mut chain = chain.lock().await;
                let faucet_account = chain.get_account(&faucet_address);
                let total_needed = FAUCET_AMOUNT.saturating_add(fee);
                if faucet_account.balance < total_needed {
                    Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        "faucet depleted: refill the configured faucet wallet".to_string(),
                    ))
                } else {
                    let mut tx = Transaction::new(
                        chain.chain_id(),
                        faucet_wallet.keypair.public_key.clone(),
                        address.clone(),
                        FAUCET_AMOUNT,
                        fee,
                        faucet_account.nonce,
                    );
                    tx.sign(&faucet_wallet.keypair);
                    match chain.add_transaction(tx.clone()) {
                        Ok(()) => Ok(tx),
                        Err(err) => Err((
                            StatusCode::BAD_REQUEST,
                            format!("faucet transfer rejected: {}", err),
                        )),
                    }
                }
            };

            let tx = match tx_or_err {
                Ok(tx) => tx,
                Err((status, msg)) => {
                    rollback_cooldowns().await;
                    return Ok(json_err(status, &msg));
                }
            };

            if let Ok(data) = bincode::serialize(&tx) {
                let _ = outbound_tx.try_send(NetworkMessage::NewTransaction(data));
            };

            // Persist cooldowns to disk now that the request has succeeded.
            // The in-memory entries were already written under the atomic
            // critical section above.
            {
                let cooldowns = faucet_cooldowns.lock().await;
                persist_faucet_cooldowns(&cooldowns);
            }
            {
                let ip_cooldowns = faucet_ip_cooldowns.lock().await;
                persist_faucet_ip_cooldowns(&ip_cooldowns);
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

        // POST /eth — Ethereum-compatible JSON-RPC subset (Metamask, ethers.js, wagmi)
        (Method::POST, ["eth"]) | (Method::POST, ["rpc", "eth"]) => {
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
            let response = eth_rpc::handle(Arc::clone(&chain), &body_bytes).await;
            let body = serde_json::to_string(&response).unwrap_or_default();
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json");
            builder = with_public_cors_headers(builder);
            Ok(finish_response(builder, body))
        }

        // GET /eth — friendly hint for browsers hitting the RPC URL
        (Method::GET, ["eth"]) => {
            let body = "CURS3D Ethereum-compatible JSON-RPC endpoint. POST a JSON-RPC 2.0 request here.\n\
                 Supported methods: eth_chainId, eth_blockNumber, eth_gasPrice,\n\
                 eth_getBalance, eth_getTransactionCount, eth_getCode, eth_getStorageAt,\n\
                 eth_getBlockByNumber, eth_getBlockByHash, eth_getTransactionByHash,\n\
                 eth_getTransactionReceipt, eth_getLogs, eth_feeHistory, eth_estimateGas,\n\
                 eth_call, eth_sendRawTransaction, net_version, web3_clientVersion, web3_sha3.\n\
                 EVM transactions use standard secp256k1-signed RLP via eth_sendRawTransaction;\n\
                 native CURS3D ML-DSA transactions use POST /api/tx/submit.\n";
            let mut builder = Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/plain; charset=utf-8");
            builder = with_public_cors_headers(builder);
            Ok(finish_response(builder, body))
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

/// Active eth_subscribe channels mapped from subscription id → kind.
/// Kind is the eth subscription type ("newHeads", "logs"). Each kind is mapped
/// to one of our internal event types when forwarding broadcast messages.
#[derive(Clone, Debug)]
struct EthSubscription {
    kind: String,
}

/// Hard cap on time we'll wait for a `ws_tx.send().await` to drain before we
/// declare the client too slow and drop the connection. Prevents a slow client
/// from accumulating an unbounded outbound buffer in tokio-tungstenite.
const WS_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Send a WebSocket message with a write-timeout. Returns Err if the timeout
/// fires or the underlying socket errors — in both cases the caller should
/// drop the connection.
async fn ws_send_bounded(
    ws_tx: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<TcpStream>,
        WsMessage,
    >,
    msg: WsMessage,
) -> Result<(), ()> {
    match tokio::time::timeout(WS_WRITE_TIMEOUT, ws_tx.send(msg)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            tracing::debug!("WebSocket send error: {}", e);
            Err(())
        }
        Err(_) => {
            tracing::warn!(
                "WebSocket client too slow ({}s write timeout) — dropping connection",
                WS_WRITE_TIMEOUT.as_secs()
            );
            Err(())
        }
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
    subscribed_events.insert("new_header".to_string());
    subscribed_events.insert("new_transaction".to_string());
    subscribed_events.insert("finality".to_string());

    // Active eth_subscribe channels (Metamask, ethers.js, wagmi). Mapped from
    // subscription id (random hex) to the requested kind ("newHeads", "logs").
    let mut eth_subs: HashMap<String, EthSubscription> = HashMap::new();
    let mut next_eth_sub_id: u64 = 1;

    loop {
        tokio::select! {
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        // First: try eth JSON-RPC subscribe / unsubscribe
                        if let Ok(rpc) = serde_json::from_str::<serde_json::Value>(&text)
                            && rpc.get("jsonrpc").and_then(|v| v.as_str()) == Some("2.0")
                            && let Some(method) = rpc.get("method").and_then(|m| m.as_str())
                        {
                            let id = rpc.get("id").cloned().unwrap_or(serde_json::Value::Null);
                            match method {
                                "eth_subscribe" => {
                                    let kind = rpc
                                        .get("params")
                                        .and_then(|p| p.as_array())
                                        .and_then(|a| a.first())
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("newHeads")
                                        .to_string();
                                    let sub_id = format!("0x{:016x}", next_eth_sub_id);
                                    next_eth_sub_id += 1;
                                    eth_subs.insert(sub_id.clone(), EthSubscription { kind });
                                    let resp = serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": sub_id,
                                    });
                                    if ws_tx
                                        .send(WsMessage::Text(resp.to_string()))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                                "eth_unsubscribe" => {
                                    let sub_id = rpc
                                        .get("params")
                                        .and_then(|p| p.as_array())
                                        .and_then(|a| a.first())
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    let removed = eth_subs.remove(sub_id).is_some();
                                    let resp = serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "result": removed,
                                    });
                                    if ws_tx
                                        .send(WsMessage::Text(resp.to_string()))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                                _ => {
                                    let resp = serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id,
                                        "error": {
                                            "code": -32601,
                                            "message": format!("ws method {} not supported (use POST /eth for non-streaming RPC)", method),
                                        }
                                    });
                                    if ws_tx
                                        .send(WsMessage::Text(resp.to_string()))
                                        .await
                                        .is_err()
                                    {
                                        break;
                                    }
                                    continue;
                                }
                            }
                        }

                        // Otherwise: legacy CURS3D subscription request
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
                        let Ok(event_json) = serde_json::from_str::<serde_json::Value>(&event_str) else { continue };
                        let event_type = event_json
                            .get("type")
                            .and_then(|t| t.as_str())
                            .unwrap_or("");

                        // Native CURS3D event stream
                        if (subscribed_events.contains(event_type) || subscribed_events.is_empty())
                            && ws_send_bounded(&mut ws_tx, WsMessage::Text(event_str.clone())).await.is_err()
                        {
                            break;
                        }

                        // ETH-style eth_subscribe notifications
                        for (sub_id, sub) in &eth_subs {
                            let matches_kind = match sub.kind.as_str() {
                                "newHeads" => event_type == "new_header",
                                "logs" => event_type == "new_block", // best-effort
                                _ => false,
                            };
                            if !matches_kind {
                                continue;
                            }
                            let payload = event_json.get("data").cloned().unwrap_or(serde_json::Value::Null);
                            let notif = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "eth_subscription",
                                "params": {
                                    "subscription": sub_id,
                                    "result": payload,
                                }
                            });
                            if ws_send_bounded(&mut ws_tx, WsMessage::Text(notif.to_string()))
                                .await
                                .is_err()
                            {
                                return;
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
    runtime_state: SharedRuntimeState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = API_START_TIME.get_or_init(Instant::now);
    let listener = TcpListener::bind(addr).await?;
    let connection_limit = Arc::new(Semaphore::new(MAX_HTTP_CONNECTIONS));
    let ws_connection_count = Arc::new(AtomicU64::new(0));
    let rate_limiter: RateLimiterMap = Arc::new(Mutex::new(HashMap::new()));
    let request_counter = Arc::new(AtomicU64::new(0));
    let faucet_cooldowns: FaucetCooldownMap = Arc::new(Mutex::new(load_faucet_cooldowns()));
    let faucet_ip_cooldowns: FaucetIpCooldownMap = Arc::new(Mutex::new(load_faucet_ip_cooldowns()));
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
        let faucet_ip_cooldowns = Arc::clone(&faucet_ip_cooldowns);
        let runtime_state = Arc::clone(&runtime_state);

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
                    faucet_ip_cooldowns: Arc::clone(&faucet_ip_cooldowns),
                    runtime_state: Arc::clone(&runtime_state),
                };
                async move { handle_request(req, chain, event_tx, outbound_tx, ctx).await }
            });

            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                tracing::warn!("HTTP connection error: {}", err);
            }
        });
    }
}
