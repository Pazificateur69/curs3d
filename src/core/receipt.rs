use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogEntry {
    pub contract: Vec<u8>,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiptLocation {
    pub block_height: u64,
    pub tx_index: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexedReceipt {
    pub tx_hash: Vec<u8>,
    pub block_height: u64,
    pub tx_index: usize,
    pub receipt: Receipt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexedLogEntry {
    pub block_height: u64,
    pub tx_index: usize,
    pub log_index: usize,
    pub tx_hash: Vec<u8>,
    pub contract: Vec<u8>,
    pub topics: Vec<Vec<u8>>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogFilter {
    #[serde(default)]
    pub contract: Option<Vec<u8>>,
    #[serde(default)]
    pub topic: Option<Vec<u8>>,
    #[serde(default)]
    pub from_block: Option<u64>,
    #[serde(default)]
    pub to_block: Option<u64>,
    #[serde(default)]
    pub limit: Option<usize>,
}
