use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore, mpsc};

use crate::core::chain::{AccountState, Blockchain, TransactionEstimate};
use crate::core::receipt::{IndexedLogEntry, IndexedReceipt, LogFilter};
use crate::core::state_proof::{AccountProof, StorageProof};
use crate::core::transaction::Transaction;
use crate::network::NetworkMessage;

const MAX_RPC_CONNECTIONS: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RpcRequest {
    SubmitTransaction {
        transaction: Transaction,
    },
    EstimateTransaction {
        transaction: Transaction,
    },
    GetAccount {
        address: Vec<u8>,
    },
    GetReceipt {
        tx_hash: Vec<u8>,
    },
    QueryLogs {
        filter: LogFilter,
    },
    GetAccountProof {
        address: Vec<u8>,
    },
    GetStorageProof {
        contract_address: Vec<u8>,
        key: Vec<u8>,
    },
    GetStatus,
}

#[derive(Debug, Serialize, Deserialize)]
struct RpcEnvelope {
    #[serde(default)]
    token: Option<String>,
    request: RpcRequest,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeStatus {
    pub chain_id: String,
    pub chain_name: String,
    pub epoch: u64,
    pub epoch_start_height: u64,
    pub height: u64,
    pub finalized_height: u64,
    pub latest_hash: String,
    pub genesis_hash: String,
    pub pending_transactions: usize,
    pub active_validators: usize,
    #[serde(default = "default_protocol_version")]
    pub protocol_version: u32,
}

fn default_protocol_version() -> u32 {
    1
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RpcResponse {
    Submitted { tx_hash: String },
    Estimated { estimate: TransactionEstimate },
    Account { state: AccountState },
    Receipt { receipt: IndexedReceipt },
    Logs { entries: Vec<IndexedLogEntry> },
    AccountProof { proof: AccountProof },
    StorageProof { proof: StorageProof },
    Status { status: NodeStatus },
    Error { message: String },
}

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("remote error: {0}")]
    Remote(String),
}

pub async fn serve(
    addr: &str,
    chain: Arc<Mutex<Blockchain>>,
    outbound_tx: mpsc::Sender<NetworkMessage>,
) -> Result<(), RpcError> {
    let listener = TcpListener::bind(addr).await?;
    let connection_limit = Arc::new(Semaphore::new(MAX_RPC_CONNECTIONS));

    loop {
        let (stream, _) = listener.accept().await?;
        let chain = Arc::clone(&chain);
        let outbound_tx = outbound_tx.clone();
        let connection_limit = Arc::clone(&connection_limit);
        tokio::spawn(async move {
            let Ok(_permit) = connection_limit.acquire_owned().await else {
                return;
            };
            if let Err(err) = handle_connection(stream, chain, outbound_tx).await {
                tracing::warn!("RPC connection error: {}", err);
            }
        });
    }
}

pub async fn send_request(addr: &str, request: &RpcRequest) -> Result<RpcResponse, RpcError> {
    let mut stream = TcpStream::connect(addr).await?;
    let envelope = RpcEnvelope {
        token: rpc_token(),
        request: request.clone(),
    };
    let payload = serde_json::to_vec(&envelope)?;
    stream.write_all(&payload).await?;
    stream.write_all(b"\n").await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Err(RpcError::Remote("empty response from node".to_string()));
    }

    let response = serde_json::from_str::<RpcResponse>(line.trim_end())?;
    Ok(response)
}

async fn handle_connection(
    stream: TcpStream,
    chain: Arc<Mutex<Blockchain>>,
    outbound_tx: mpsc::Sender<NetworkMessage>,
) -> Result<(), RpcError> {
    let (reader_half, mut writer_half) = stream.into_split();
    let mut reader = BufReader::new(reader_half);
    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Ok(());
    }

    let request = parse_request(line.trim_end())?;
    let response = handle_request(request, chain, outbound_tx).await;
    let json = serde_json::to_string(&response)?;
    writer_half.write_all(json.as_bytes()).await?;
    writer_half.write_all(b"\n").await?;
    Ok(())
}

fn rpc_token() -> Option<String> {
    std::env::var("CURS3D_RPC_TOKEN")
        .ok()
        .filter(|value| !value.is_empty())
}

fn parse_request(payload: &str) -> Result<RpcRequest, RpcError> {
    if let Ok(envelope) = serde_json::from_str::<RpcEnvelope>(payload) {
        if let Some(expected) = rpc_token()
            && envelope.token.as_deref() != Some(expected.as_str())
        {
            return Err(RpcError::Remote("missing or invalid RPC token".to_string()));
        }
        return Ok(envelope.request);
    }

    if rpc_token().is_some() {
        return Err(RpcError::Remote("missing or invalid RPC token".to_string()));
    }

    Ok(serde_json::from_str::<RpcRequest>(payload)?)
}

async fn handle_request(
    request: RpcRequest,
    chain: Arc<Mutex<Blockchain>>,
    outbound_tx: mpsc::Sender<NetworkMessage>,
) -> RpcResponse {
    match request {
        RpcRequest::SubmitTransaction { transaction } => {
            let tx_hash = transaction.hash_hex();
            let mut chain = chain.lock().await;
            match chain.add_transaction(transaction) {
                Ok(()) => {
                    if let Some(tx) = chain.pending_transactions.last()
                        && let Ok(data) = bincode::serialize(tx)
                    {
                        let _ = outbound_tx.try_send(NetworkMessage::NewTransaction(data));
                    }
                    RpcResponse::Submitted { tx_hash }
                }
                Err(err) => RpcResponse::Error {
                    message: err.to_string(),
                },
            }
        }
        RpcRequest::EstimateTransaction { transaction } => {
            let chain = chain.lock().await;
            match chain.estimate_transaction(&transaction) {
                Ok(estimate) => RpcResponse::Estimated { estimate },
                Err(err) => RpcResponse::Error {
                    message: err.to_string(),
                },
            }
        }
        RpcRequest::GetAccount { address } => {
            let chain = chain.lock().await;
            RpcResponse::Account {
                state: chain.get_account(&address),
            }
        }
        RpcRequest::GetReceipt { tx_hash } => {
            let chain = chain.lock().await;
            match chain.get_receipt(&tx_hash) {
                Some(receipt) => RpcResponse::Receipt { receipt },
                None => RpcResponse::Error {
                    message: "receipt not found".to_string(),
                },
            }
        }
        RpcRequest::QueryLogs { filter } => {
            let chain = chain.lock().await;
            RpcResponse::Logs {
                entries: chain.query_logs(&filter),
            }
        }
        RpcRequest::GetAccountProof { address } => {
            let chain = chain.lock().await;
            match chain.get_account_proof(&address) {
                Some(proof) => RpcResponse::AccountProof { proof },
                None => RpcResponse::Error {
                    message: "account proof not found".to_string(),
                },
            }
        }
        RpcRequest::GetStorageProof {
            contract_address,
            key,
        } => {
            let chain = chain.lock().await;
            match chain.get_storage_proof(&contract_address, &key) {
                Some(proof) => RpcResponse::StorageProof { proof },
                None => RpcResponse::Error {
                    message: "storage proof not found".to_string(),
                },
            }
        }
        RpcRequest::GetStatus => {
            let chain = chain.lock().await;
            RpcResponse::Status {
                status: NodeStatus {
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
                },
            }
        }
    }
}
