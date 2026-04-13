use std::sync::Arc;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex};

use crate::core::block::Block;
use crate::core::chain::Blockchain;
use crate::core::transaction::{Transaction, TransactionKind};
use crate::crypto::hash;

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
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header("Access-Control-Allow-Headers", "Content-Type")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

fn json_err(status: StatusCode, msg: &str) -> Response<Full<Bytes>> {
    let resp: ApiResponse<()> = ApiResponse {
        ok: false,
        data: None,
        error: Some(msg.to_string()),
    };
    let body = serde_json::to_string(&resp).unwrap_or_default();
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header("Access-Control-Allow-Headers", "Content-Type")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

fn cors_preflight() -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header("Access-Control-Allow-Headers", "Content-Type")
        .body(Full::new(Bytes::new()))
        .unwrap()
}

// ─── Serializable API Structs ────────────────────────────────────────

#[derive(Serialize)]
struct ApiStatus {
    chain_name: String,
    height: u64,
    finalized_height: u64,
    latest_hash: String,
    genesis_hash: String,
    pending_transactions: usize,
    active_validators: usize,
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
struct ApiValidator {
    address: String,
    public_key: String,
    stake: u64,
}

#[derive(Serialize)]
struct ApiFaucetResult {
    message: String,
    address: String,
    amount: u64,
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
        },
        from: hex::encode(&tx.from),
        to: hex::encode(&tx.to),
        amount: tx.amount,
        fee: tx.fee,
        nonce: tx.nonce,
        timestamp: tx.timestamp,
    }
}

// ─── Request Router ──────────────────────────────────────────────────

async fn handle_request(
    req: Request<Incoming>,
    chain: Arc<Mutex<Blockchain>>,
    event_tx: broadcast::Sender<String>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    if req.method() == Method::OPTIONS {
        return Ok(cors_preflight());
    }

    let path = req.uri().path().to_string();
    let method = req.method().clone();

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    match (method, segments.as_slice()) {
        // GET /api/status
        (Method::GET, ["api", "status"]) => {
            let chain = chain.lock().await;
            Ok(json_ok(ApiStatus {
                chain_name: chain.genesis_config.chain_name.clone(),
                height: chain.height(),
                finalized_height: chain.finalized_height(),
                latest_hash: hex::encode(chain.latest_hash()),
                genesis_hash: hex::encode(chain.genesis_hash()),
                pending_transactions: chain.pending_transactions.len(),
                active_validators: chain.active_validator_count(),
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
            let params: Vec<(&str, &str)> = query
                .split('&')
                .filter_map(|p| p.split_once('='))
                .collect();

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

        // GET /api/faucet/:address (testnet only)
        (Method::GET, ["api", "faucet", addr_hex]) => {
            let addr_clean = addr_hex.strip_prefix("CUR").unwrap_or(addr_hex);
            let address = match hex::decode(addr_clean) {
                Ok(a) if a.len() == hash::ADDRESS_LEN => a,
                _ => return Ok(json_err(StatusCode::BAD_REQUEST, "invalid address")),
            };

            let faucet_amount: u64 = 100_000_000; // 100 CUR

            // Directly credit the account (testnet faucet only)
            let mut chain = chain.lock().await;
            let account = chain.accounts.entry(address.clone()).or_default();
            account.balance = account.balance.saturating_add(faucet_amount);

            Ok(json_ok(ApiFaucetResult {
                message: "Faucet tokens sent".to_string(),
                address: hex::encode(&address),
                amount: faucet_amount,
            }))
        }

        // POST /api/tx/submit
        (Method::POST, ["api", "tx", "submit"]) => {
            let body_bytes = match http_body_util::BodyExt::collect(req.into_body()).await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => return Ok(json_err(StatusCode::BAD_REQUEST, "failed to read body")),
            };

            let tx: Transaction = match serde_json::from_slice(&body_bytes) {
                Ok(t) => t,
                Err(e) => {
                    return Ok(json_err(
                        StatusCode::BAD_REQUEST,
                        &format!("invalid transaction JSON: {}", e),
                    ))
                }
            };

            let tx_hash = tx.hash_hex();
            let mut chain = chain.lock().await;
            match chain.add_transaction(tx) {
                Ok(()) => {
                    let event =
                        serde_json::json!({"type": "new_tx", "hash": tx_hash}).to_string();
                    let _ = event_tx.send(event);
                    Ok(json_ok(serde_json::json!({"tx_hash": tx_hash})))
                }
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
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("HTTP API listening on http://{}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);
        let chain = Arc::clone(&chain);
        let event_tx = event_tx.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let chain = Arc::clone(&chain);
                let event_tx = event_tx.clone();
                async move { handle_request(req, chain, event_tx).await }
            });

            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                tracing::warn!("HTTP connection error: {}", err);
            }
        });
    }
}
