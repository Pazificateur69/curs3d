use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub tx_hash: Vec<u8>,
    pub success: bool,
    pub gas_used: u64,
    pub logs: Vec<LogEntry>,
    pub return_data: Vec<u8>,
    pub contract_address: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub contract: Vec<u8>,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}
