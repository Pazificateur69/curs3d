use futures::StreamExt;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder, gossipsub, mdns, noise, swarm::SwarmEvent, tcp, yamux,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::consensus::{EquivocationEvidence, FinalityVote};
use crate::core::block::Block;
use crate::core::chain::{Blockchain, ChainError};
use crate::crypto::dilithium::{self, KeyPair, Signature};

const SYNC_TIMEOUT_SECS: u64 = 15;
const MAX_SYNC_RETRIES: u32 = 3;
const MAX_SEEN_BLOCKS: usize = 1000;
const SYNC_BATCH_SIZE: u64 = 50;

// ─── Network Messages ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    NewBlock(Vec<u8>),
    NewTransaction(Vec<u8>),
    RequestBlocks {
        from_height: u64,
        requester_peer_id: String,
        expected_prev_hash: Vec<u8>,
        genesis_hash: Vec<u8>,
    },
    BlockResponse {
        from_height: u64,
        target_peer_id: String,
        responder_peer_id: String,
        genesis_hash: Vec<u8>,
        blocks: Vec<Vec<u8>>,
    },
    /// Signed height announcement — only verified announces trigger sync
    HeightAnnounce {
        height: u64,
        latest_hash: Vec<u8>,
        genesis_hash: Vec<u8>,
        peer_id: String,
        /// Optional: public key + signature for verified announces
        public_key: Option<Vec<u8>>,
        signature: Option<Signature>,
    },
    /// Equivocation evidence — provable slashing
    SlashingEvidence(Vec<u8>),
    /// Finality vote from a validator
    FinalityVote(Vec<u8>),
}

// ─── Behaviour ───────────────────────────────────────────────────────

#[derive(NetworkBehaviour)]
pub struct CursBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
}

// ─── Network Node ────────────────────────────────────────────────────

pub struct NetworkNode {
    pub peer_id: PeerId,
    pub swarm: Swarm<CursBehaviour>,
    pub topic: gossipsub::IdentTopic,
}

impl NetworkNode {
    pub async fn new(port: u16, bootnodes: &[String]) -> Result<Self, Box<dyn std::error::Error>> {
        let topic = gossipsub::IdentTopic::new("curs3d-mainnet");

        let mut swarm = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_behaviour(|key| {
                let gossipsub_config = gossipsub::ConfigBuilder::default()
                    .heartbeat_interval(Duration::from_secs(10))
                    .validation_mode(gossipsub::ValidationMode::Strict)
                    .max_transmit_size(10 * 1024 * 1024)
                    .build()
                    .map_err(|e| std::io::Error::other(e.to_string()))?;

                let gossipsub = gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossipsub_config,
                )?;

                let mdns = mdns::tokio::Behaviour::new(
                    mdns::Config::default(),
                    key.public().to_peer_id(),
                )?;

                Ok(CursBehaviour { gossipsub, mdns })
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        swarm.behaviour_mut().gossipsub.subscribe(&topic)?;

        let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", port).parse()?;
        swarm.listen_on(listen_addr)?;

        for bootnode in bootnodes {
            match bootnode.parse::<Multiaddr>() {
                Ok(addr) => {
                    if let Err(err) = swarm.dial(addr.clone()) {
                        warn!("Failed to dial bootnode {}: {}", addr, err);
                    } else {
                        info!("Dialing bootnode {}", addr);
                    }
                }
                Err(err) => warn!("Ignoring invalid bootnode {}: {}", bootnode, err),
            }
        }

        let peer_id = *swarm.local_peer_id();
        info!("Node started with PeerId: {}", peer_id);

        Ok(NetworkNode {
            peer_id,
            swarm,
            topic,
        })
    }

    pub fn broadcast(
        &mut self,
        message: &NetworkMessage,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let data = serde_json::to_vec(message)?;
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(self.topic.clone(), data)?;
        Ok(())
    }

    // ─── Main Event Loop ─────────────────────────────────────────────

    pub async fn run_with_chain(
        &mut self,
        chain: Arc<Mutex<Blockchain>>,
        mut outbound_rx: mpsc::Receiver<NetworkMessage>,
        validator_key: Option<KeyPair>,
    ) {
        let mut discovered_peers: HashSet<PeerId> = HashSet::new();

        // Sync state
        let mut sync_requested = false;
        let mut sync_deadline: Option<Instant> = None;
        let mut sync_retries: u32 = 0;

        // Peer height tracking
        let mut peer_heights: HashMap<String, (u64, Vec<u8>)> = HashMap::new();

        // Block deduplication cache
        let mut seen_block_hashes: HashSet<Vec<u8>> = HashSet::new();

        // Timers
        let mut block_timer = tokio::time::interval(Duration::from_secs(10));
        let mut announce_timer = tokio::time::interval(Duration::from_secs(30));

        loop {
            // Check sync timeout
            if let Some(deadline) = sync_deadline {
                if Instant::now() >= deadline {
                    sync_retries += 1;
                    if sync_retries >= MAX_SYNC_RETRIES {
                        info!("Sync timed out after {} retries. Resetting.", MAX_SYNC_RETRIES);
                        sync_requested = false;
                        sync_deadline = None;
                        sync_retries = 0;
                    } else {
                        info!("Sync timeout, retry {}/{}", sync_retries, MAX_SYNC_RETRIES);
                        let chain_lock = chain.lock().await;
                        let msg = NetworkMessage::RequestBlocks {
                            from_height: chain_lock.height() + 1,
                            requester_peer_id: self.peer_id.to_string(),
                            expected_prev_hash: chain_lock.latest_hash().to_vec(),
                            genesis_hash: chain_lock.genesis_hash().to_vec(),
                        };
                        drop(chain_lock);
                        let _ = self.broadcast(&msg);
                        sync_deadline =
                            Some(Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS));
                    }
                }
            }

            tokio::select! {
                Some(msg) = outbound_rx.recv() => {
                    if let Err(e) = self.broadcast(&msg) {
                        warn!("Failed to broadcast: {}", e);
                    }
                }

                // Block production
                _ = block_timer.tick() => {
                    if let Some(ref keypair) = validator_key {
                        let maybe_block = {
                            let chain_lock = chain.lock().await;
                            chain_lock.create_block(keypair)
                        };

                        match maybe_block {
                            Ok(block) => {
                                let block_hash = block.hash_hex();
                                let block_height = block.header.height;
                                let serialized = match bincode::serialize(&block) {
                                    Ok(data) => data,
                                    Err(e) => {
                                        error!("Failed to serialize block: {}", e);
                                        continue;
                                    }
                                };

                                let add_result = {
                                    let mut chain_lock = chain.lock().await;
                                    chain_lock.add_block(block.clone())
                                };

                                match add_result {
                                    Ok(()) => {
                                        info!("Produced block #{} ({})", block_height, &block_hash[..16]);

                                        // Broadcast block
                                        let msg = NetworkMessage::NewBlock(serialized);
                                        if let Err(e) = self.broadcast(&msg) {
                                            warn!("Failed to broadcast block: {}", e);
                                        }

                                        // Cast finality vote
                                        let vote = FinalityVote::new(
                                            block.hash.clone(),
                                            block.header.height,
                                            keypair,
                                        );
                                        if let Ok(vote_data) = bincode::serialize(&vote) {
                                            // Apply locally
                                            {
                                                let mut chain_lock = chain.lock().await;
                                                chain_lock.add_finality_vote(vote);
                                            }
                                            let msg = NetworkMessage::FinalityVote(vote_data);
                                            let _ = self.broadcast(&msg);
                                        }

                                        seen_block_hashes.insert(block.hash);
                                    }
                                    Err(ChainError::UnauthorizedValidator) => {}
                                    Err(e) => error!("Failed to add own block: {}", e),
                                }
                            }
                            Err(ChainError::UnauthorizedValidator) => {}
                            Err(e) => error!("Failed to create block: {}", e),
                        }
                    }
                }

                // Periodic height announcement
                _ = announce_timer.tick() => {
                    let chain_lock = chain.lock().await;
                    let height = chain_lock.height();
                    let latest_hash = chain_lock.latest_hash().to_vec();
                    let genesis_hash = chain_lock.genesis_hash().to_vec();
                    drop(chain_lock);

                    let (public_key, signature) = if let Some(kp) = &validator_key {
                        let mut data = height.to_le_bytes().to_vec();
                        data.extend_from_slice(&latest_hash);
                        data.extend_from_slice(&genesis_hash);
                        let sig = kp.sign(&data);
                        (Some(kp.public_key.clone()), Some(sig))
                    } else {
                        (None, None)
                    };

                    let msg = NetworkMessage::HeightAnnounce {
                        height,
                        latest_hash,
                        genesis_hash,
                        peer_id: self.peer_id.to_string(),
                        public_key,
                        signature,
                    };
                    let _ = self.broadcast(&msg);
                }

                // Network events
                event = self.swarm.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(CursBehaviourEvent::Gossipsub(
                            gossipsub::Event::Message { message, .. }
                        )) => {
                            if let Ok(net_msg) = serde_json::from_slice::<NetworkMessage>(&message.data) {
                                match net_msg {
                                    NetworkMessage::NewBlock(data) => {
                                        Self::handle_new_block(
                                            &chain,
                                            &data,
                                            &mut seen_block_hashes,
                                            &validator_key,
                                            self,
                                        ).await;
                                    }
                                    NetworkMessage::NewTransaction(data) => {
                                        Self::handle_new_transaction(&chain, &data).await;
                                    }
                                    NetworkMessage::RequestBlocks {
                                        from_height,
                                        requester_peer_id,
                                        expected_prev_hash,
                                        genesis_hash,
                                    } => {
                                        self.handle_block_request(
                                            &chain,
                                            from_height,
                                            &requester_peer_id,
                                            &expected_prev_hash,
                                            &genesis_hash,
                                        ).await;
                                    }
                                    NetworkMessage::BlockResponse {
                                        from_height,
                                        target_peer_id,
                                        responder_peer_id: _,
                                        genesis_hash,
                                        blocks: blocks_data,
                                    } => {
                                        if target_peer_id == self.peer_id.to_string() {
                                            Self::handle_block_response(
                                                &chain,
                                                from_height,
                                                &genesis_hash,
                                                &blocks_data,
                                                &mut sync_requested,
                                                &mut sync_deadline,
                                                &mut sync_retries,
                                            ).await;
                                        }
                                    }
                                    NetworkMessage::HeightAnnounce {
                                        height,
                                        latest_hash,
                                        genesis_hash,
                                        peer_id: announce_peer_id,
                                        public_key,
                                        signature,
                                    } => {
                                        // Verify signature if present
                                        let verified = match (&public_key, &signature) {
                                            (Some(pk), Some(sig)) => {
                                                let mut data = height.to_le_bytes().to_vec();
                                                data.extend_from_slice(&latest_hash);
                                                data.extend_from_slice(&genesis_hash);
                                                dilithium::verify(&data, sig, pk)
                                            }
                                            _ => false,
                                        };

                                        // Track peer height (verified or not, for awareness)
                                        peer_heights.insert(
                                            announce_peer_id.clone(),
                                            (height, latest_hash.clone()),
                                        );

                                        let chain_lock = chain.lock().await;
                                        let our_height = chain_lock.height();
                                        let our_genesis = chain_lock.genesis_hash().to_vec();
                                        drop(chain_lock);

                                        if genesis_hash != our_genesis {
                                            continue; // Different chain
                                        }

                                        // Only trigger sync from verified announces
                                        if height > our_height && !sync_requested && verified {
                                            info!(
                                                "Verified peer {} at height {} (we: {}). Syncing...",
                                                &announce_peer_id, height, our_height
                                            );
                                            let chain_lock = chain.lock().await;
                                            let msg = NetworkMessage::RequestBlocks {
                                                from_height: our_height + 1,
                                                requester_peer_id: self.peer_id.to_string(),
                                                expected_prev_hash: chain_lock.latest_hash().to_vec(),
                                                genesis_hash: our_genesis,
                                            };
                                            drop(chain_lock);
                                            let _ = self.broadcast(&msg);
                                            sync_requested = true;
                                            sync_deadline = Some(
                                                Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS),
                                            );
                                        } else if height > our_height && !sync_requested && !verified {
                                            // Unverified announce — still sync but log it
                                            info!(
                                                "Unverified peer {} at height {} (we: {}). Syncing...",
                                                &announce_peer_id, height, our_height
                                            );
                                            let chain_lock = chain.lock().await;
                                            let msg = NetworkMessage::RequestBlocks {
                                                from_height: our_height + 1,
                                                requester_peer_id: self.peer_id.to_string(),
                                                expected_prev_hash: chain_lock.latest_hash().to_vec(),
                                                genesis_hash: our_genesis,
                                            };
                                            drop(chain_lock);
                                            let _ = self.broadcast(&msg);
                                            sync_requested = true;
                                            sync_deadline = Some(
                                                Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS),
                                            );
                                        }
                                    }
                                    NetworkMessage::SlashingEvidence(data) => {
                                        if let Ok(evidence) = bincode::deserialize::<EquivocationEvidence>(&data) {
                                            let mut chain_lock = chain.lock().await;
                                            match chain_lock.process_equivocation(&evidence) {
                                                Ok(penalty) => {
                                                    info!(
                                                        "Slashed validator for equivocation at height {}. Penalty: {}",
                                                        evidence.height, penalty
                                                    );
                                                }
                                                Err(e) => {
                                                    warn!("Rejected slashing evidence: {}", e);
                                                }
                                            }
                                        }
                                    }
                                    NetworkMessage::FinalityVote(data) => {
                                        if let Ok(vote) = bincode::deserialize::<crate::consensus::FinalityVote>(&data) {
                                            let mut chain_lock = chain.lock().await;
                                            if let Some(finalized) = chain_lock.add_finality_vote(vote) {
                                                info!(
                                                    "Block #{} finalized via network vote",
                                                    finalized.height
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        SwarmEvent::Behaviour(CursBehaviourEvent::Mdns(
                            mdns::Event::Discovered(peers)
                        )) => {
                            for (peer_id, _addr) in peers {
                                if discovered_peers.insert(peer_id) {
                                    info!("Discovered peer: {}", peer_id);
                                    self.swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                                }
                            }
                        }
                        SwarmEvent::Behaviour(CursBehaviourEvent::Mdns(
                            mdns::Event::Expired(peers)
                        )) => {
                            for (peer_id, _addr) in peers {
                                info!("Peer expired: {}", peer_id);
                                discovered_peers.remove(&peer_id);
                                self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                            }
                        }
                        SwarmEvent::NewListenAddr { address, .. } => {
                            info!("Listening on {}", address);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // ─── Message Handlers ────────────────────────────────────────────

    async fn handle_new_block(
        chain: &Arc<Mutex<Blockchain>>,
        data: &[u8],
        seen_hashes: &mut HashSet<Vec<u8>>,
        validator_key: &Option<KeyPair>,
        node: &mut Self,
    ) {
        // Dedup: hash the raw data
        let data_hash = crate::crypto::hash::sha3_hash(data);
        if seen_hashes.contains(&data_hash) {
            return;
        }
        if seen_hashes.len() >= MAX_SEEN_BLOCKS {
            seen_hashes.clear();
        }
        seen_hashes.insert(data_hash);

        let block = match bincode::deserialize::<Block>(data) {
            Ok(b) => b,
            Err(e) => {
                warn!("Failed to deserialize block: {}", e);
                return;
            }
        };

        let block_height = block.header.height;
        let block_hash = block.hash.clone();

        let mut chain_lock = chain.lock().await;

        // Try adding normally first, then try fork choice
        match chain_lock.add_block(block.clone()) {
            Ok(()) => {
                info!("Accepted block #{} from network", block_height);

                // Cast finality vote if we're a validator
                if let Some(kp) = &validator_key {
                    let vote = FinalityVote::new(block_hash, block_height, kp);
                    if let Ok(vote_data) = bincode::serialize(&vote) {
                        chain_lock.add_finality_vote(vote);
                        drop(chain_lock);
                        let msg = NetworkMessage::FinalityVote(vote_data);
                        let _ = node.broadcast(&msg);
                    }
                }
            }
            Err(ChainError::InvalidHeight { .. }) | Err(ChainError::InvalidPrevHash) => {
                // This might be a fork — try fork choice
                match chain_lock.add_block_with_fork_choice(block.clone()) {
                    Ok(reorged) => {
                        if reorged {
                            info!("Reorg to block #{} from network", block_height);
                        } else {
                            info!("Fork block #{} stored (not canonical)", block_height);
                        }
                    }
                    Err(e) => {
                        // Check for equivocation: same height, same validator, different hash
                        if let Some(our_block) =
                            chain_lock.blocks.get(block_height as usize)
                        {
                            if our_block.header.validator_public_key
                                == block.header.validator_public_key
                                && our_block.hash != block.hash
                            {
                                if let (Some(sig_a), Some(sig_b)) =
                                    (&our_block.signature, &block.signature)
                                {
                                    let evidence = EquivocationEvidence {
                                        height: block_height,
                                        validator_public_key: block
                                            .header
                                            .validator_public_key
                                            .clone(),
                                        block_hash_a: our_block.hash.clone(),
                                        signature_a: sig_a.clone(),
                                        block_hash_b: block.hash.clone(),
                                        signature_b: sig_b.clone(),
                                    };
                                    if evidence.verify() {
                                        warn!(
                                            "EQUIVOCATION detected at height {} by validator",
                                            block_height
                                        );
                                        let _ = chain_lock.process_equivocation(&evidence);
                                        if let Ok(ev_data) = bincode::serialize(&evidence) {
                                            drop(chain_lock);
                                            let msg =
                                                NetworkMessage::SlashingEvidence(ev_data);
                                            let _ = node.broadcast(&msg);
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        warn!("Rejected block #{}: {}", block_height, e);
                    }
                }
            }
            Err(e) => {
                warn!("Rejected block #{}: {}", block_height, e);
            }
        }
    }

    async fn handle_new_transaction(chain: &Arc<Mutex<Blockchain>>, data: &[u8]) {
        match bincode::deserialize::<crate::core::transaction::Transaction>(data) {
            Ok(tx) => {
                let mut chain_lock = chain.lock().await;
                match chain_lock.add_transaction(tx) {
                    Ok(()) => info!("Accepted transaction from network"),
                    Err(e) => warn!("Rejected transaction: {}", e),
                }
            }
            Err(e) => warn!("Failed to deserialize transaction: {}", e),
        }
    }

    async fn handle_block_request(
        &mut self,
        chain: &Arc<Mutex<Blockchain>>,
        from_height: u64,
        requester_peer_id: &str,
        expected_prev_hash: &[u8],
        request_genesis_hash: &[u8],
    ) {
        let chain_lock = chain.lock().await;
        let our_height = chain_lock.height();
        let our_genesis = chain_lock.genesis_hash();

        if request_genesis_hash != our_genesis {
            return;
        }
        if from_height > our_height {
            return;
        }
        if from_height > 0 {
            if let Some(prev_block) = chain_lock.blocks.get((from_height - 1) as usize) {
                if prev_block.hash != expected_prev_hash {
                    return;
                }
            }
        }

        let end_height = std::cmp::min(from_height + SYNC_BATCH_SIZE - 1, our_height);
        let mut blocks_data = Vec::new();

        for h in from_height..=end_height {
            if let Some(block) = chain_lock.blocks.get(h as usize) {
                if let Ok(serialized) = bincode::serialize(block) {
                    blocks_data.push(serialized);
                }
            }
        }
        drop(chain_lock);

        if !blocks_data.is_empty() {
            info!(
                "Sending {} blocks ({}..{}) to {}",
                blocks_data.len(),
                from_height,
                end_height,
                requester_peer_id
            );
            let chain_lock = chain.lock().await;
            let msg = NetworkMessage::BlockResponse {
                from_height,
                target_peer_id: requester_peer_id.to_string(),
                responder_peer_id: self.peer_id.to_string(),
                genesis_hash: chain_lock.genesis_hash().to_vec(),
                blocks: blocks_data,
            };
            drop(chain_lock);
            let _ = self.broadcast(&msg);
        }
    }

    async fn handle_block_response(
        chain: &Arc<Mutex<Blockchain>>,
        from_height: u64,
        response_genesis_hash: &[u8],
        blocks_data: &[Vec<u8>],
        sync_requested: &mut bool,
        sync_deadline: &mut Option<Instant>,
        sync_retries: &mut u32,
    ) {
        let mut chain_lock = chain.lock().await;

        if response_genesis_hash != chain_lock.genesis_hash() {
            return;
        }
        if from_height != chain_lock.height() + 1 {
            return;
        }

        let mut accepted = 0u64;
        for data in blocks_data {
            match bincode::deserialize::<Block>(data) {
                Ok(block) => {
                    let height = block.header.height;
                    match chain_lock.add_block(block) {
                        Ok(()) => accepted += 1,
                        Err(e) => {
                            warn!("Sync: rejected block #{}: {}", height, e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    warn!("Sync: failed to deserialize block: {}", e);
                    break;
                }
            }
        }

        if accepted > 0 {
            info!(
                "Synced {} blocks. Height: {}",
                accepted,
                chain_lock.height()
            );
            *sync_requested = false;
            *sync_deadline = None;
            *sync_retries = 0;
        }
    }
}
