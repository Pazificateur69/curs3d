use futures::StreamExt;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder, gossipsub, identity, mdns, noise, swarm::SwarmEvent,
    tcp, yamux,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::consensus::{EquivocationEvidence, FinalityVote};
use crate::core::block::Block;
use crate::core::chain::{Blockchain, ChainError};
use crate::crypto::dilithium::{self, KeyPair, Signature};
use crate::storage::{SnapshotManifest, StateChunk};

const SYNC_TIMEOUT_SECS: u64 = 15;
const MAX_SYNC_RETRIES: u32 = 3;
const MAX_SEEN_BLOCKS: usize = 1000;
const SYNC_BATCH_SIZE: u64 = 50;

// ─── P2P Rate Limiting ──────────────────────────────────────────────

const PEER_RATE_LIMIT_WINDOW_SECS: u64 = 10;
const PEER_MAX_MESSAGES_PER_WINDOW: usize = 500;
const PEER_BAN_DURATION_SECS: u64 = 300;
const PEER_MAX_TRACKED: usize = 2000;
const PEER_CLEANUP_INTERVAL_SECS: u64 = 60;

struct PeerRateState {
    message_timestamps: VecDeque<Instant>,
    banned_until: Option<Instant>,
    total_violations: u32,
}

impl PeerRateState {
    fn new() -> Self {
        Self {
            message_timestamps: VecDeque::new(),
            banned_until: None,
            total_violations: 0,
        }
    }
}

struct PeerRateLimiter {
    peers: HashMap<PeerId, PeerRateState>,
}

impl PeerRateLimiter {
    fn new() -> Self {
        Self {
            peers: HashMap::new(),
        }
    }

    /// Check if a message from this peer should be allowed.
    /// Returns `true` if allowed, `false` if rate-limited or banned.
    fn check(&mut self, peer_id: &PeerId) -> bool {
        let now = Instant::now();
        let window = Duration::from_secs(PEER_RATE_LIMIT_WINDOW_SECS);

        let state = self
            .peers
            .entry(*peer_id)
            .or_insert_with(PeerRateState::new);

        // Check if peer is banned
        if let Some(ban_until) = state.banned_until {
            if now < ban_until {
                return false;
            }
            // Ban expired — clear it
            state.banned_until = None;
        }

        // Evict timestamps outside the window
        while state
            .message_timestamps
            .front()
            .is_some_and(|ts| now.duration_since(*ts) > window)
        {
            state.message_timestamps.pop_front();
        }

        // Check rate limit
        if state.message_timestamps.len() >= PEER_MAX_MESSAGES_PER_WINDOW {
            state.total_violations += 1;
            // Ban on first violation
            let ban_multiplier = state.total_violations as u64;
            state.banned_until =
                Some(now + Duration::from_secs(PEER_BAN_DURATION_SECS * ban_multiplier));
            warn!(
                "P2P rate limit: banning peer {} for {}s (violation #{})",
                peer_id,
                PEER_BAN_DURATION_SECS * ban_multiplier,
                state.total_violations,
            );
            return false;
        }

        state.message_timestamps.push_back(now);
        true
    }

    /// Returns `true` if the peer is currently banned.
    fn is_banned(&self, peer_id: &PeerId) -> bool {
        self.peers.get(peer_id).is_some_and(|state| {
            state
                .banned_until
                .is_some_and(|ban_until| Instant::now() < ban_until)
        })
    }

    /// Remove stale entries (peers with no recent activity and no active ban).
    fn cleanup(&mut self) {
        let now = Instant::now();
        let window = Duration::from_secs(PEER_RATE_LIMIT_WINDOW_SECS * 6);
        self.peers.retain(|_, state| {
            // Keep if banned
            if state.banned_until.is_some_and(|t| now < t) {
                return true;
            }
            // Keep if recent activity
            state
                .message_timestamps
                .back()
                .is_some_and(|ts| now.duration_since(*ts) < window)
        });

        // Hard cap to prevent unbounded growth
        if self.peers.len() > PEER_MAX_TRACKED {
            let excess = self.peers.len() - PEER_MAX_TRACKED;
            let keys_to_remove: Vec<PeerId> = self
                .peers
                .iter()
                .filter(|(_, state)| state.banned_until.is_none())
                .take(excess)
                .map(|(k, _)| *k)
                .collect();
            for key in keys_to_remove {
                self.peers.remove(&key);
            }
        }
    }
}

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
        /// Protocol version the peer is running
        #[serde(default = "default_protocol_version")]
        protocol_version: u32,
    },
    /// Equivocation evidence — provable slashing
    SlashingEvidence(Vec<u8>),
    /// Finality vote from a validator
    FinalityVote(Vec<u8>),
    /// Request a state sync snapshot from a peer
    RequestSnapshot {
        requester_peer_id: String,
        #[serde(default)]
        preferred_height: Option<u64>,
        #[serde(default)]
        start_chunk: usize,
        #[serde(default)]
        known_finalized_height: u64,
        #[serde(default)]
        known_finalized_hash: Vec<u8>,
    },
    /// Snapshot manifest (bincode-serialized SnapshotManifest)
    SnapshotManifest {
        target_peer_id: String,
        data: Vec<u8>,
    },
    /// Snapshot chunk (bincode-serialized StateChunk)
    SnapshotChunk {
        target_peer_id: String,
        height: u64,
        data: Vec<u8>,
    },
}

fn default_protocol_version() -> u32 {
    1
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

pub fn topic_name(chain_id: &str, protocol_version: u32) -> String {
    format!("curs3d-{}-v{}", chain_id, protocol_version)
}

impl NetworkNode {
    pub async fn new(
        port: u16,
        bootnodes: &[String],
        topic_name: &str,
        identity_keypair: identity::Keypair,
        public_addrs: &[Multiaddr],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let topic = gossipsub::IdentTopic::new(topic_name);

        let mut swarm = SwarmBuilder::with_existing_identity(identity_keypair)
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
        for addr in public_addrs {
            swarm.add_external_address(addr.clone());
        }

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

    fn switch_topic(&mut self, topic_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let new_topic = gossipsub::IdentTopic::new(topic_name);
        if self.topic.hash() == new_topic.hash() {
            return Ok(());
        }
        let _ = self
            .swarm
            .behaviour_mut()
            .gossipsub
            .unsubscribe(&self.topic);
        self.swarm.behaviour_mut().gossipsub.subscribe(&new_topic)?;
        self.topic = new_topic;
        Ok(())
    }

    fn next_missing_chunk_index(
        manifest: &SnapshotManifest,
        chunks: &HashMap<usize, StateChunk>,
    ) -> usize {
        for index in 0..manifest.chunk_count {
            if !chunks.contains_key(&index) {
                return index;
            }
        }
        manifest.chunk_count
    }

    // ─── Main Event Loop ─────────────────────────────────────────────

    pub async fn run_with_chain(
        &mut self,
        chain: Arc<Mutex<Blockchain>>,
        mut outbound_rx: mpsc::Receiver<NetworkMessage>,
        validator_key: Option<KeyPair>,
        event_tx: Option<tokio::sync::broadcast::Sender<String>>,
    ) {
        let mut discovered_peers: HashSet<PeerId> = HashSet::new();

        // P2P rate limiter
        let mut rate_limiter = PeerRateLimiter::new();
        let mut rate_limiter_cleanup_timer =
            tokio::time::interval(Duration::from_secs(PEER_CLEANUP_INTERVAL_SECS));

        // Sync state
        let mut sync_requested = false;
        let mut sync_deadline: Option<Instant> = None;
        let mut sync_retries: u32 = 0;

        // Peer height tracking
        let mut peer_heights: HashMap<String, (u64, Vec<u8>)> = HashMap::new();
        let mut pending_snapshot_manifest: Option<SnapshotManifest> = None;
        let mut pending_snapshot_chunks: HashMap<usize, StateChunk> = HashMap::new();

        // Block deduplication cache
        let mut seen_block_hashes: HashSet<Vec<u8>> = HashSet::new();

        // Timers
        let mut block_timer = tokio::time::interval(Duration::from_secs(10));
        let mut announce_timer = tokio::time::interval(Duration::from_secs(30));

        loop {
            // Check sync timeout
            if let Some(deadline) = sync_deadline
                && Instant::now() >= deadline
            {
                sync_retries += 1;
                if sync_retries >= MAX_SYNC_RETRIES {
                    info!(
                        "Sync timed out after {} retries. Resetting.",
                        MAX_SYNC_RETRIES
                    );
                    sync_requested = false;
                    sync_deadline = None;
                    sync_retries = 0;
                } else {
                    info!("Sync timeout, retry {}/{}", sync_retries, MAX_SYNC_RETRIES);
                    let chain_lock = chain.lock().await;
                    let msg = if let Some(manifest) = pending_snapshot_manifest.as_ref() {
                        NetworkMessage::RequestSnapshot {
                            requester_peer_id: self.peer_id.to_string(),
                            preferred_height: Some(manifest.height),
                            start_chunk: Self::next_missing_chunk_index(
                                manifest,
                                &pending_snapshot_chunks,
                            ),
                            known_finalized_height: chain_lock.finalized_height(),
                            known_finalized_hash: chain_lock
                                .finality_tracker
                                .finalized_hash
                                .clone(),
                        }
                    } else {
                        NetworkMessage::RequestBlocks {
                            from_height: chain_lock.height() + 1,
                            requester_peer_id: self.peer_id.to_string(),
                            expected_prev_hash: chain_lock.latest_hash().to_vec(),
                            genesis_hash: chain_lock.genesis_hash().to_vec(),
                        }
                    };
                    drop(chain_lock);
                    let _ = self.broadcast(&msg);
                    sync_deadline = Some(Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS));
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
                    let (chain_id, protocol_version) = {
                        let chain_lock = chain.lock().await;
                        (
                            chain_lock.chain_id().to_string(),
                            chain_lock.protocol_version_at_height(chain_lock.height()),
                        )
                    };
                    if let Err(err) = self.switch_topic(&topic_name(&chain_id, protocol_version)) {
                        warn!("Failed to switch network topic: {}", err);
                    }

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

                                        // Emit WebSocket event
                                        if let Some(etx) = &event_tx {
                                            let _ = etx.send(serde_json::json!({
                                                "type": "new_block",
                                                "data": {
                                                    "height": block_height,
                                                    "hash": &block_hash,
                                                    "tx_count": block.transactions.len(),
                                                    "timestamp": block.header.timestamp,
                                                }
                                            }).to_string());
                                        }

                                        // Broadcast block
                                        let msg = NetworkMessage::NewBlock(serialized);
                                        if let Err(e) = self.broadcast(&msg) {
                                            warn!("Failed to broadcast block: {}", e);
                                        }

                                        // Cast finality vote
                                        let vote_epoch = {
                                            let chain_lock = chain.lock().await;
                                            chain_lock.epoch_for_height(block.header.height)
                                        };
                                        let vote = FinalityVote::new(
                                            block.hash.clone(),
                                            block.header.height,
                                            vote_epoch,
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
                    let (chain_id, height, latest_hash, genesis_hash, protocol_version) = {
                        let chain_lock = chain.lock().await;
                        (
                            chain_lock.chain_id().to_string(),
                            chain_lock.height(),
                            chain_lock.latest_hash().to_vec(),
                            chain_lock.genesis_hash().to_vec(),
                            chain_lock.protocol_version_at_height(chain_lock.height()),
                        )
                    };
                    if let Err(err) = self.switch_topic(&topic_name(&chain_id, protocol_version)) {
                        warn!("Failed to switch network topic: {}", err);
                    }

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
                        protocol_version,
                    };
                    let _ = self.broadcast(&msg);
                }

                // Rate limiter cleanup
                _ = rate_limiter_cleanup_timer.tick() => {
                    rate_limiter.cleanup();
                }

                // Network events
                event = self.swarm.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(CursBehaviourEvent::Gossipsub(
                            gossipsub::Event::Message { message, .. }
                        )) => {
                            // P2P rate limiting: check the propagation source
                            if let Some(source_peer) = message.source
                                && !rate_limiter.check(&source_peer)
                            {
                                continue;
                            }
                            if let Ok(net_msg) = serde_json::from_slice::<NetworkMessage>(&message.data) {
                                match net_msg {
                                    NetworkMessage::NewBlock(data) => {
                                        Self::handle_new_block(
                                            &chain,
                                            &data,
                                            &mut seen_block_hashes,
                                            &validator_key,
                                            self,
                                            &event_tx,
                                        ).await;
                                    }
                                    NetworkMessage::NewTransaction(data) => {
                                        Self::handle_new_transaction(&chain, &data, &event_tx).await;
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
                                        protocol_version: peer_protocol_version,
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

                                        // Reject peers with unknown/incompatible protocol version
                                        let our_protocol_version = {
                                            let chain_lock = chain.lock().await;
                                            chain_lock.protocol_version_at_height(our_height)
                                        };
                                        if peer_protocol_version != our_protocol_version {
                                            warn!(
                                                "Peer {} running protocol v{} (we: v{}). Ignoring incompatible peer.",
                                                &announce_peer_id, peer_protocol_version, our_protocol_version
                                            );
                                            continue;
                                        }

                                        // Only trigger sync from verified announces
                                        if height > our_height && !sync_requested && verified {
                                            info!(
                                                "Verified peer {} at height {} (we: {}). Syncing...",
                                                &announce_peer_id, height, our_height
                                            );
                                            let msg = if height.saturating_sub(our_height) > SYNC_BATCH_SIZE {
                                                let chain_lock = chain.lock().await;
                                                NetworkMessage::RequestSnapshot {
                                                    requester_peer_id: self.peer_id.to_string(),
                                                    preferred_height: None,
                                                    start_chunk: 0,
                                                    known_finalized_height: chain_lock.finalized_height(),
                                                    known_finalized_hash: chain_lock.finality_tracker.finalized_hash.clone(),
                                                }
                                            } else {
                                                let chain_lock = chain.lock().await;
                                                let msg = NetworkMessage::RequestBlocks {
                                                    from_height: our_height + 1,
                                                    requester_peer_id: self.peer_id.to_string(),
                                                    expected_prev_hash: chain_lock.latest_hash().to_vec(),
                                                    genesis_hash: our_genesis,
                                                };
                                                drop(chain_lock);
                                                msg
                                            };
                                            let _ = self.broadcast(&msg);
                                            sync_requested = true;
                                            sync_deadline = Some(
                                                Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS),
                                            );
                                        } else if height > our_height && !sync_requested && !verified {
                                            info!(
                                                "Ignoring unverified peer {} at height {} (we: {}).",
                                                &announce_peer_id, height, our_height
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
                                    NetworkMessage::RequestSnapshot {
                                        requester_peer_id,
                                        preferred_height,
                                        start_chunk,
                                        known_finalized_height,
                                        known_finalized_hash,
                                    } => {
                                        let chain_lock = chain.lock().await;
                                        if known_finalized_height > 0 {
                                            let checkpoint_ok = chain_lock
                                                .blocks
                                                .get(known_finalized_height as usize)
                                                .map(|block| block.hash == known_finalized_hash)
                                                .unwrap_or(false);
                                            if !checkpoint_ok {
                                                warn!(
                                                    "Ignoring snapshot request from {}: checkpoint mismatch at height {}",
                                                    requester_peer_id,
                                                    known_finalized_height
                                                );
                                                continue;
                                            }
                                        }
                                        let manifest = chain_lock
                                            .create_snapshot()
                                            .ok()
                                            .filter(|manifest| {
                                                preferred_height
                                                    .is_none_or(|height| manifest.height == height)
                                            })
                                            .or_else(|| chain_lock.create_snapshot().ok());
                                        if let Some(manifest) = manifest {
                                            let snapshot_height = manifest.height;
                                            if let (Ok(data), Ok(chunks)) = (
                                                bincode::serialize(&manifest),
                                                chain_lock.get_snapshot_chunks(snapshot_height),
                                            ) {
                                                drop(chain_lock);
                                                let msg = NetworkMessage::SnapshotManifest {
                                                    target_peer_id: requester_peer_id.clone(),
                                                    data,
                                                };
                                                let _ = self.broadcast(&msg);
                                                for chunk in chunks.into_iter().skip(start_chunk) {
                                                    if let Ok(data) = bincode::serialize(&chunk) {
                                                        let _ = self.broadcast(&NetworkMessage::SnapshotChunk {
                                                            target_peer_id: requester_peer_id.clone(),
                                                            height: snapshot_height,
                                                            data,
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    NetworkMessage::SnapshotManifest { target_peer_id, data } => {
                                        if target_peer_id != self.peer_id.to_string() {
                                            continue;
                                        }
                                        match bincode::deserialize::<SnapshotManifest>(&data) {
                                            Ok(manifest) => {
                                                info!("Received snapshot manifest for height {}", manifest.height);
                                                let same_session = pending_snapshot_manifest
                                                    .as_ref()
                                                    .is_some_and(|current| {
                                                        current.height == manifest.height
                                                            && current.chunk_root == manifest.chunk_root
                                                    });
                                                if !same_session {
                                                    pending_snapshot_chunks.clear();
                                                }
                                                pending_snapshot_manifest = Some(manifest);
                                            }
                                            Err(err) => warn!("Failed to deserialize snapshot manifest: {}", err),
                                        }
                                    }
                                    NetworkMessage::SnapshotChunk { target_peer_id, height, data } => {
                                        if target_peer_id != self.peer_id.to_string() {
                                            continue;
                                        }
                                        let Some(manifest) = pending_snapshot_manifest.clone() else {
                                            continue;
                                        };
                                        if manifest.height != height {
                                            continue;
                                        }
                                        match bincode::deserialize::<StateChunk>(&data) {
                                            Ok(chunk) => {
                                                pending_snapshot_chunks.insert(chunk.index, chunk);
                                                if pending_snapshot_chunks.len() == manifest.chunk_count {
                                                    let mut ordered = Vec::with_capacity(manifest.chunk_count);
                                                    let mut complete = true;
                                                    for index in 0..manifest.chunk_count {
                                                        if let Some(chunk) = pending_snapshot_chunks.remove(&index) {
                                                            ordered.push(chunk);
                                                        } else {
                                                            complete = false;
                                                            break;
                                                        }
                                                    }
                                                    if complete {
                                                        let mut chain_lock = chain.lock().await;
                                                        match chain_lock.apply_snapshot(&manifest, &ordered) {
                                                            Ok(()) => {
                                                                info!("Applied snapshot at height {}", manifest.height);
                                                                tracing::info!(
                                                                    target: "audit",
                                                                    event = "snapshot_sync_applied",
                                                                    height = manifest.height,
                                                                    chunk_count = manifest.chunk_count,
                                                                );
                                                                if manifest.tip_height > manifest.height {
                                                                    let request = NetworkMessage::RequestBlocks {
                                                                        from_height: manifest.height.saturating_add(1),
                                                                        requester_peer_id: self.peer_id.to_string(),
                                                                        expected_prev_hash: chain_lock.latest_hash().to_vec(),
                                                                        genesis_hash: chain_lock.genesis_hash().to_vec(),
                                                                    };
                                                                    let _ = self.broadcast(&request);
                                                                    sync_requested = true;
                                                                    sync_deadline = Some(
                                                                        Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS),
                                                                    );
                                                                } else {
                                                                    sync_requested = false;
                                                                    sync_deadline = None;
                                                                    sync_retries = 0;
                                                                }
                                                            }
                                                            Err(err) => {
                                                                warn!("Failed to apply snapshot: {}", err);
                                                                pending_snapshot_chunks.clear();
                                                            }
                                                        }
                                                        pending_snapshot_manifest = None;
                                                    }
                                                }
                                            }
                                            Err(err) => warn!("Failed to deserialize snapshot chunk: {}", err),
                                        }
                                    }
                                }
                            }
                        }
                        SwarmEvent::Behaviour(CursBehaviourEvent::Mdns(
                            mdns::Event::Discovered(peers)
                        )) => {
                            for (peer_id, _addr) in peers {
                                // Skip banned peers
                                if rate_limiter.is_banned(&peer_id) {
                                    warn!("Ignoring banned peer {}", peer_id);
                                    continue;
                                }
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
                        SwarmEvent::ExternalAddrConfirmed { address } => {
                            info!("Confirmed public address {}", address);
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
        event_tx: &Option<tokio::sync::broadcast::Sender<String>>,
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

                // Emit WebSocket event
                if let Some(etx) = &event_tx {
                    let _ = etx.send(
                        serde_json::json!({
                            "type": "new_block",
                            "data": {
                                "height": block_height,
                                "hash": hex::encode(&block_hash),
                                "tx_count": block.transactions.len(),
                                "timestamp": block.header.timestamp,
                            }
                        })
                        .to_string(),
                    );
                }

                // Cast finality vote if we're a validator
                if let Some(kp) = &validator_key {
                    let vote = FinalityVote::new(
                        block_hash,
                        block_height,
                        block_height / chain_lock.epoch_length.max(1),
                        kp,
                    );
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
                        if let Some(our_block) = chain_lock.blocks.get(block_height as usize)
                            && our_block.header.validator_public_key
                                == block.header.validator_public_key
                            && our_block.hash != block.hash
                            && let (Some(sig_a), Some(sig_b)) =
                                (&our_block.signature, &block.signature)
                        {
                            let evidence = EquivocationEvidence {
                                height: block_height,
                                validator_public_key: block.header.validator_public_key.clone(),
                                block_header_a: our_block.header.clone(),
                                block_hash_a: our_block.hash.clone(),
                                signature_a: sig_a.clone(),
                                block_header_b: block.header.clone(),
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
                                    let msg = NetworkMessage::SlashingEvidence(ev_data);
                                    let _ = node.broadcast(&msg);
                                    return;
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

    async fn handle_new_transaction(
        chain: &Arc<Mutex<Blockchain>>,
        data: &[u8],
        event_tx: &Option<tokio::sync::broadcast::Sender<String>>,
    ) {
        match bincode::deserialize::<crate::core::transaction::Transaction>(data) {
            Ok(tx) => {
                let tx_hash = hex::encode(crate::crypto::hash::sha3_hash(
                    &bincode::serialize(&tx).unwrap_or_default(),
                ));
                let kind = format!("{:?}", tx.kind);
                let mut chain_lock = chain.lock().await;
                match chain_lock.add_transaction(tx) {
                    Ok(()) => {
                        info!("Accepted transaction from network");
                        if let Some(etx) = &event_tx {
                            let _ = etx.send(
                                serde_json::json!({
                                    "type": "new_transaction",
                                    "data": {
                                        "hash": tx_hash,
                                        "kind": kind,
                                    }
                                })
                                .to_string(),
                            );
                        }
                    }
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
        if from_height > 0
            && let Some(prev_block) = chain_lock.blocks.get((from_height - 1) as usize)
            && prev_block.hash != expected_prev_hash
        {
            return;
        }

        let end_height = std::cmp::min(from_height + SYNC_BATCH_SIZE - 1, our_height);
        let mut blocks_data = Vec::new();

        for h in from_height..=end_height {
            if let Some(block) = chain_lock.blocks.get(h as usize)
                && let Ok(serialized) = bincode::serialize(block)
            {
                blocks_data.push(serialized);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_peer_id(n: u8) -> PeerId {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        let key = identity::Keypair::ed25519_from_bytes(bytes).unwrap();
        key.public().to_peer_id()
    }

    #[test]
    fn test_rate_limiter_allows_normal_traffic() {
        let mut limiter = PeerRateLimiter::new();
        let peer = test_peer_id(1);
        // Should allow messages under the limit
        for _ in 0..100 {
            assert!(limiter.check(&peer));
        }
    }

    #[test]
    fn test_rate_limiter_blocks_flood() {
        let mut limiter = PeerRateLimiter::new();
        let peer = test_peer_id(2);
        // Fill up to the limit
        for _ in 0..PEER_MAX_MESSAGES_PER_WINDOW {
            assert!(limiter.check(&peer));
        }
        // Next message should be rejected and peer banned
        assert!(!limiter.check(&peer));
        assert!(limiter.is_banned(&peer));
    }

    #[test]
    fn test_rate_limiter_does_not_affect_other_peers() {
        let mut limiter = PeerRateLimiter::new();
        let peer_a = test_peer_id(3);
        let peer_b = test_peer_id(4);
        // Exhaust peer_a
        for _ in 0..PEER_MAX_MESSAGES_PER_WINDOW {
            limiter.check(&peer_a);
        }
        assert!(!limiter.check(&peer_a));
        // peer_b should still be allowed
        assert!(limiter.check(&peer_b));
        assert!(!limiter.is_banned(&peer_b));
    }

    #[test]
    fn test_rate_limiter_cleanup_removes_stale() {
        let mut limiter = PeerRateLimiter::new();
        let peer = test_peer_id(5);
        limiter.check(&peer);
        // After cleanup, peer with no ban and stale timestamps gets removed
        // (can't easily test time-based cleanup in unit test, but test the method doesn't panic)
        limiter.cleanup();
        // Peer should still work after cleanup
        assert!(limiter.check(&peer));
    }

    #[test]
    fn test_rate_limiter_escalating_bans() {
        let mut limiter = PeerRateLimiter::new();
        let peer = test_peer_id(6);
        // First violation
        for _ in 0..PEER_MAX_MESSAGES_PER_WINDOW {
            limiter.check(&peer);
        }
        assert!(!limiter.check(&peer));
        let state = limiter.peers.get(&peer).unwrap();
        assert_eq!(state.total_violations, 1);

        // Clear ban manually to test escalation
        limiter.peers.get_mut(&peer).unwrap().banned_until = None;
        limiter
            .peers
            .get_mut(&peer)
            .unwrap()
            .message_timestamps
            .clear();

        // Second violation
        for _ in 0..PEER_MAX_MESSAGES_PER_WINDOW {
            limiter.check(&peer);
        }
        assert!(!limiter.check(&peer));
        let state = limiter.peers.get(&peer).unwrap();
        assert_eq!(state.total_violations, 2);
    }
}
