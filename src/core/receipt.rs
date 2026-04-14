use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub tx_hash: Vec<u8>,
    pub success: bool,
    pub gas_used: u64,
    #[serde(default)]
    pub effective_gas_price: u64,
    #[serde(default)]
    pub priority_fee_paid: u64,
    #[serde(default)]
    pub base_fee_burned: u64,
    #[serde(default)]
    pub gas_refunded: u64,
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
