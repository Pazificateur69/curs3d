use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContractState {
    pub code_hash: Vec<u8>,
    pub code: Vec<u8>,
    pub storage: HashMap<Vec<u8>, Vec<u8>>,
    pub owner: Vec<u8>,
}
