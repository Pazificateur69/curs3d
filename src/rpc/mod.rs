use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::core::chain::{AccountState, Blockchain};
use crate::core::transaction::Transaction;

#[derive(Debug, Serialize, Deserialize)]
pub enum RpcRequest {
    SubmitTransaction { transaction: Transaction },
    GetAccount { address: Vec<u8> },
    GetStatus,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeStatus {
    pub chain_name: String,
    pub height: u64,
    pub finalized_height: u64,
    pub latest_hash: String,
    pub genesis_hash: String,
    pub pending_transactions: usize,
    pub active_validators: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RpcResponse {
    Submitted { tx_hash: String },
    Account { state: AccountState },
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

pub async fn serve(addr: &str, chain: Arc<Mutex<Blockchain>>) -> Result<(), RpcError> {
    let listener = TcpListener::bind(addr).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let chain = Arc::clone(&chain);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(stream, chain).await {
                tracing::warn!("RPC connection error: {}", err);
            }
        });
    }
}

pub async fn send_request(addr: &str, request: &RpcRequest) -> Result<RpcResponse, RpcError> {
    let mut stream = TcpStream::connect(addr).await?;
    let payload = serde_json::to_vec(request)?;
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
) -> Result<(), RpcError> {
    let (reader_half, mut writer_half) = stream.into_split();
    let mut reader = BufReader::new(reader_half);
    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Ok(());
    }

    let request = serde_json::from_str::<RpcRequest>(line.trim_end())?;
    let response = handle_request(request, chain).await;
    let json = serde_json::to_string(&response)?;
    writer_half.write_all(json.as_bytes()).await?;
    writer_half.write_all(b"\n").await?;
    Ok(())
}

async fn handle_request(request: RpcRequest, chain: Arc<Mutex<Blockchain>>) -> RpcResponse {
    match request {
        RpcRequest::SubmitTransaction { transaction } => {
            let tx_hash = transaction.hash_hex();
            let mut chain = chain.lock().await;
            match chain.add_transaction(transaction) {
                Ok(()) => RpcResponse::Submitted { tx_hash },
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
        RpcRequest::GetStatus => {
            let chain = chain.lock().await;
            RpcResponse::Status {
                status: NodeStatus {
                    chain_name: chain.genesis_config.chain_name.clone(),
                    height: chain.height(),
                    finalized_height: chain.finalized_height(),
                    latest_hash: hex::encode(chain.latest_hash()),
                    genesis_hash: hex::encode(chain.genesis_hash()),
                    pending_transactions: chain.pending_transactions.len(),
                    active_validators: chain.active_validator_count(),
                },
            }
        }
    }
}
