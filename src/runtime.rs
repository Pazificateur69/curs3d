use serde::Serialize;
use tokio::sync::RwLock;

use std::sync::Arc;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Relay,
    Validator,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeSnapshot {
    pub role: NodeRole,
    pub validator_address: Option<String>,
    pub network_online: bool,
    pub peer_count: usize,
    pub bootnode_count: usize,
    pub rpc_addr: String,
    pub http_addr: String,
}

#[derive(Debug)]
pub struct RuntimeState {
    role: NodeRole,
    validator_address: Option<String>,
    network_online: bool,
    peer_count: usize,
    bootnode_count: usize,
    rpc_addr: String,
    http_addr: String,
}

impl RuntimeState {
    pub fn new(
        role: NodeRole,
        validator_address: Option<String>,
        bootnode_count: usize,
        rpc_addr: impl Into<String>,
        http_addr: impl Into<String>,
    ) -> Self {
        Self {
            role,
            validator_address,
            network_online: false,
            peer_count: 0,
            bootnode_count,
            rpc_addr: rpc_addr.into(),
            http_addr: http_addr.into(),
        }
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        RuntimeSnapshot {
            role: self.role.clone(),
            validator_address: self.validator_address.clone(),
            network_online: self.network_online,
            peer_count: self.peer_count,
            bootnode_count: self.bootnode_count,
            rpc_addr: self.rpc_addr.clone(),
            http_addr: self.http_addr.clone(),
        }
    }

    pub fn set_network_online(&mut self, online: bool) {
        self.network_online = online;
    }

    pub fn set_peer_count(&mut self, peer_count: usize) {
        self.peer_count = peer_count;
    }
}

pub type SharedRuntimeState = Arc<RwLock<RuntimeState>>;
