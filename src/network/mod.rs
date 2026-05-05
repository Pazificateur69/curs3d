use bincode::Options;
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

use crate::consensus::{
    BACKUP_LEADER_TIMEOUT_SECS, EquivocationEvidence, FinalityVote, allowed_backup_rank,
};
use crate::core::block::Block;
use crate::core::chain::{Blockchain, ChainError};
use crate::crypto::dilithium::{self, KeyPair, Signature};
use crate::runtime::SharedRuntimeState;
use crate::storage::{SnapshotManifest, StateChunk};

/// Per-request sync deadline. A sender that legitimately has 50 blocks to
/// serialize, sign-verify, JSON-encode and stream over a slow link can take
/// longer than the historical 15s, especially with state-heavy blocks
/// (large transactions or receipt logs). 30s gives the realistic worst case
/// room without making cold-start on a healthy network feel sluggish.
const SYNC_TIMEOUT_SECS: u64 = 30;
const MAX_SYNC_RETRIES: u32 = 3;
const MAX_SEEN_BLOCKS: usize = 1000;
const SYNC_BATCH_SIZE: u64 = 50;
const STARTUP_GRACE_SECS: u64 = 20;
const PEERLESS_PRODUCTION_AFTER_SECS: u64 = 120;
const PEER_MESH_SETTLE_SECS: u64 = 15;
const VERIFIED_TIP_TTL_SECS: u64 = 120;
const REBROADCAST_INTERVAL_SECS: u64 = 5;
const MAX_PENDING_BROADCASTS: usize = 256;
/// Maximum size for any deserialized P2P message (16 MB) — prevents OOM from malicious payloads
const MAX_DESERIALIZE_SIZE: u64 = 16 * 1024 * 1024;

/// Bounded bincode deserialization to prevent OOM attacks from untrusted network data.
///
/// CRITICAL: must match the encoding used by `bincode::serialize` (the default
/// "legacy" encoding: fixed-int, little-endian, allow-trailing). `bincode::options()`
/// alone returns `DefaultOptions` which uses **var-int** encoding — mixing the two
/// produces silent schema mismatches that surface as "string is not valid utf8"
/// errors deep in nested types. See: bincode v1 docs.
fn bounded_deserialize<T: serde::de::DeserializeOwned>(data: &[u8]) -> Result<T, String> {
    if data.len() as u64 > MAX_DESERIALIZE_SIZE {
        return Err(format!("payload too large: {} bytes", data.len()));
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_little_endian()
        .allow_trailing_bytes()
        .with_limit(MAX_DESERIALIZE_SIZE)
        .deserialize(data)
        .map_err(|e| format!("deserialize error: {}", e))
}

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

// ─── Peer Scoring ───────────────────────────────────────────────────

// Score constants and thresholds
const PEER_SCORE_INITIAL: i64 = 100;
#[allow(dead_code)]
const PEER_SCORE_MAX: i64 = 200;
const PEER_SCORE_MIN: i64 = -100;
const PEER_SCORE_BAN_THRESHOLD: i64 = -50;
const PEER_SCORE_DECAY_PER_TICK: i64 = 1;
#[allow(dead_code)]
const SCORE_VALID_BLOCK: i64 = 5;
#[allow(dead_code)]
const SCORE_VALID_TX: i64 = 1;
#[allow(dead_code)]
const SCORE_VALID_VOTE: i64 = 3;
#[allow(dead_code)]
const SCORE_INVALID_BLOCK: i64 = -20;
#[allow(dead_code)]
const SCORE_INVALID_TX: i64 = -5;
#[allow(dead_code)]
const SCORE_INVALID_MESSAGE: i64 = -10;
const SCORE_RATE_LIMITED: i64 = -15;

struct PeerScore {
    score: i64,
    #[allow(dead_code)]
    valid_messages: u64,
    #[allow(dead_code)]
    invalid_messages: u64,
    last_updated: Instant,
}

impl PeerScore {
    fn new() -> Self {
        Self {
            score: PEER_SCORE_INITIAL,
            valid_messages: 0,
            invalid_messages: 0,
            last_updated: Instant::now(),
        }
    }
}

struct PeerScorer {
    scores: HashMap<PeerId, PeerScore>,
}

impl PeerScorer {
    fn new() -> Self {
        Self {
            scores: HashMap::new(),
        }
    }

    /// Record a positive behavior from a peer.
    #[allow(dead_code)]
    fn record_good(&mut self, peer_id: &PeerId, points: i64) {
        let entry = self.scores.entry(*peer_id).or_insert_with(PeerScore::new);
        entry.score = (entry.score + points).min(PEER_SCORE_MAX);
        entry.valid_messages += 1;
        entry.last_updated = Instant::now();
    }

    /// Record a negative behavior from a peer.
    fn record_bad(&mut self, peer_id: &PeerId, points: i64) {
        let entry = self.scores.entry(*peer_id).or_insert_with(PeerScore::new);
        entry.score = (entry.score + points).max(PEER_SCORE_MIN);
        entry.invalid_messages += 1;
        entry.last_updated = Instant::now();
    }

    /// Check if a peer should be banned based on their score.
    fn should_ban(&self, peer_id: &PeerId) -> bool {
        self.scores
            .get(peer_id)
            .is_some_and(|s| s.score <= PEER_SCORE_BAN_THRESHOLD)
    }

    /// Get the current score of a peer.
    #[allow(dead_code)]
    fn get_score(&self, peer_id: &PeerId) -> i64 {
        self.scores
            .get(peer_id)
            .map(|s| s.score)
            .unwrap_or(PEER_SCORE_INITIAL)
    }

    /// Decay scores toward the initial value over time. Call periodically.
    fn decay_scores(&mut self) {
        for score in self.scores.values_mut() {
            if score.score > PEER_SCORE_INITIAL {
                score.score = (score.score - PEER_SCORE_DECAY_PER_TICK).max(PEER_SCORE_INITIAL);
            } else if score.score < PEER_SCORE_INITIAL {
                score.score = (score.score + PEER_SCORE_DECAY_PER_TICK).min(PEER_SCORE_INITIAL);
            }
        }
    }

    /// Remove stale peer entries.
    fn cleanup(&mut self) {
        let now = Instant::now();
        let stale = Duration::from_secs(600); // 10 min without activity
        self.scores.retain(|_, s| {
            now.duration_since(s.last_updated) < stale || s.score <= PEER_SCORE_BAN_THRESHOLD
        });
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

    fn enqueue_pending_broadcast(pending: &mut VecDeque<NetworkMessage>, message: NetworkMessage) {
        if pending.len() >= MAX_PENDING_BROADCASTS {
            pending.pop_front();
        }
        pending.push_back(message);
    }

    fn broadcast_or_queue(
        &mut self,
        message: &NetworkMessage,
        pending: &mut VecDeque<NetworkMessage>,
        context: &str,
    ) {
        if let Err(err) = self.broadcast(message) {
            warn!(
                "Failed to broadcast {}: {}. Queuing for retry.",
                context, err
            );
            Self::enqueue_pending_broadcast(pending, message.clone());
        }
    }

    fn flush_pending_broadcasts(&mut self, pending: &mut VecDeque<NetworkMessage>) {
        if pending.is_empty() {
            return;
        }

        let mut remaining = VecDeque::new();
        let queued = pending.len();
        while let Some(message) = pending.pop_front() {
            if let Err(err) = self.broadcast(&message) {
                tracing::debug!("Pending broadcast still not ready: {}", err);
                remaining.push_back(message);
            }
        }

        let sent = queued.saturating_sub(remaining.len());
        if sent > 0 {
            info!("Flushed {} queued network broadcasts", sent);
        }
        *pending = remaining;
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

    fn should_delay_initial_production(
        started_at: Instant,
        connected_peers: usize,
        _current_height: u64,
        now: Instant,
    ) -> bool {
        let elapsed = now
            .checked_duration_since(started_at)
            .unwrap_or_else(|| Duration::from_secs(0));
        if elapsed < Duration::from_secs(STARTUP_GRACE_SECS) {
            return true;
        }
        connected_peers == 0 && elapsed < Duration::from_secs(PEERLESS_PRODUCTION_AFTER_SECS)
    }

    fn allowed_rank_for_slot(
        next_height: u64,
        parent_timestamp: i64,
        _node_started_unix: i64,
        now_unix: i64,
        snapshot_size: usize,
    ) -> u32 {
        if next_height <= 1 {
            return 0;
        }
        allowed_backup_rank(parent_timestamp, now_unix, snapshot_size)
    }

    fn prune_verified_peer_tips(tips: &mut HashMap<String, (u64, Vec<u8>, Instant)>, now: Instant) {
        tips.retain(|_, (_, _, seen_at)| {
            now.checked_duration_since(*seen_at)
                .unwrap_or_else(|| Duration::from_secs(0))
                < Duration::from_secs(VERIFIED_TIP_TTL_SECS)
        });
    }

    fn sync_needed_from_verified_tips(
        tips: &HashMap<String, (u64, Vec<u8>, Instant)>,
        our_height: u64,
        our_hash: &[u8],
    ) -> Option<(u64, Vec<u8>, bool)> {
        let mut best_higher: Option<(u64, Vec<u8>)> = None;
        let mut same_height_divergent: Option<(u64, Vec<u8>)> = None;
        for (height, hash, _) in tips.values() {
            if *height > our_height {
                if best_higher
                    .as_ref()
                    .is_none_or(|(best_height, _)| height > best_height)
                {
                    best_higher = Some((*height, hash.clone()));
                }
            } else if *height == our_height && hash != our_hash && same_height_divergent.is_none() {
                same_height_divergent = Some((*height, hash.clone()));
            }
        }
        best_higher
            .map(|(height, hash)| (height, hash, false))
            .or_else(|| same_height_divergent.map(|(height, hash)| (height, hash, true)))
    }

    // ─── Main Event Loop ─────────────────────────────────────────────

    pub async fn run_with_chain(
        &mut self,
        chain: Arc<Mutex<Blockchain>>,
        mut outbound_rx: mpsc::Receiver<NetworkMessage>,
        validator_key: Option<KeyPair>,
        event_tx: Option<tokio::sync::broadcast::Sender<String>>,
        runtime_state: SharedRuntimeState,
    ) {
        let mut discovered_peers: HashSet<PeerId> = HashSet::new();

        // P2P rate limiter + peer scoring
        let mut rate_limiter = PeerRateLimiter::new();
        let mut peer_scorer = PeerScorer::new();
        let mut rate_limiter_cleanup_timer =
            tokio::time::interval(Duration::from_secs(PEER_CLEANUP_INTERVAL_SECS));

        // Sync state
        let mut sync_requested = false;
        let mut sync_deadline: Option<Instant> = None;
        let mut sync_retries: u32 = 0;

        // Peer height tracking
        let mut peer_heights: HashMap<String, (u64, Vec<u8>)> = HashMap::new();
        let mut verified_peer_tips: HashMap<String, (u64, Vec<u8>, Instant)> = HashMap::new();
        let mut pending_snapshot_manifest: Option<SnapshotManifest> = None;
        let mut pending_snapshot_chunks: HashMap<usize, StateChunk> = HashMap::new();
        let mut pending_broadcasts: VecDeque<NetworkMessage> = VecDeque::new();

        // Block deduplication cache
        let mut seen_block_hashes: HashSet<Vec<u8>> = HashSet::new();

        // Timers
        let node_started_at = Instant::now();
        let node_started_unix = chrono::Utc::now().timestamp();
        let mut last_peer_change_at = node_started_at;
        let mut block_timer = tokio::time::interval(Duration::from_secs(10));
        let mut announce_timer = tokio::time::interval(Duration::from_secs(30));
        let mut rebroadcast_timer =
            tokio::time::interval(Duration::from_secs(REBROADCAST_INTERVAL_SECS));

        loop {
            // Check sync timeout
            if let Some(deadline) = sync_deadline
                && Instant::now() >= deadline
            {
                sync_retries += 1;
                if sync_retries >= MAX_SYNC_RETRIES {
                    info!(
                        "Sync timed out after {} retries. Escalating to snapshot sync.",
                        MAX_SYNC_RETRIES
                    );
                    let chain_lock = chain.lock().await;
                    let start_chunk = pending_snapshot_manifest
                        .as_ref()
                        .map(|manifest| {
                            Self::next_missing_chunk_index(manifest, &pending_snapshot_chunks)
                        })
                        .unwrap_or(0);
                    let msg = NetworkMessage::RequestSnapshot {
                        requester_peer_id: self.peer_id.to_string(),
                        preferred_height: None,
                        start_chunk,
                        known_finalized_height: chain_lock.finalized_height(),
                        known_finalized_hash: chain_lock.finality_tracker.finalized_hash.clone(),
                    };
                    drop(chain_lock);
                    self.broadcast_or_queue(&msg, &mut pending_broadcasts, "snapshot sync request");
                    sync_requested = true;
                    sync_deadline = Some(Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS));
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
                    self.broadcast_or_queue(&msg, &mut pending_broadcasts, "sync retry");
                    sync_deadline = Some(Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS));
                }
            }

            tokio::select! {
                Some(msg) = outbound_rx.recv() => {
                    self.broadcast_or_queue(&msg, &mut pending_broadcasts, "outbound message");
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
                        if sync_requested {
                            tracing::debug!("Block production paused while sync is in flight");
                            continue;
                        }

                        // Slot-leader gate: only the elected proposer for this
                        // height may produce. If we're not the rank-0 leader
                        // we wait. After BACKUP_LEADER_TIMEOUT_SECS without a
                        // block at the expected height, the rank-1 backup may
                        // step in; rank-2 after another timeout, etc.
                        let my_address =
                            crate::crypto::hash::address_bytes_from_public_key(&keypair.public_key);
                        let (next_height, latest_hash, parent_ts, snapshot_size, current_height) = {
                            let chain_lock = chain.lock().await;
                            let parent = chain_lock.latest_block();
                            (
                                parent.header.height + 1,
                                parent.hash.clone(),
                                parent.header.timestamp,
                                chain_lock
                                    .get_epoch_snapshot(
                                        chain_lock.epoch_for_height(parent.header.height + 1),
                                    )
                                    .map(|s| s.validators.len())
                                    .unwrap_or(0),
                                chain_lock.height(),
                            )
                        };
                        let connected_peers = self.swarm.connected_peers().count();
                        let now_instant = Instant::now();
                        let mesh_settling = now_instant
                            .checked_duration_since(last_peer_change_at)
                            .unwrap_or_else(|| Duration::from_secs(0))
                            < Duration::from_secs(PEER_MESH_SETTLE_SECS);
                        if mesh_settling {
                            tracing::debug!(
                                "Block production paused while peer mesh settles (connected_peers={})",
                                connected_peers,
                            );
                            continue;
                        }
                        if Self::should_delay_initial_production(
                            node_started_at,
                            connected_peers,
                            current_height,
                            now_instant,
                        ) {
                            tracing::debug!(
                                "Initial block production delayed: height={}, connected_peers={}",
                                current_height,
                                connected_peers,
                            );
                            continue;
                        }

                        Self::prune_verified_peer_tips(&mut verified_peer_tips, now_instant);
                        if let Some((peer_height, _peer_hash, same_height_divergent)) =
                            Self::sync_needed_from_verified_tips(
                                &verified_peer_tips,
                                current_height,
                                &latest_hash,
                            )
                        {
                            let chain_lock = chain.lock().await;
                            let msg = if same_height_divergent
                                || peer_height.saturating_sub(current_height) > SYNC_BATCH_SIZE
                            {
                                NetworkMessage::RequestSnapshot {
                                    requester_peer_id: self.peer_id.to_string(),
                                    preferred_height: None,
                                    start_chunk: 0,
                                    known_finalized_height: chain_lock.finalized_height(),
                                    known_finalized_hash: chain_lock
                                        .finality_tracker
                                        .finalized_hash
                                        .clone(),
                                }
                            } else {
                                NetworkMessage::RequestBlocks {
                                    from_height: current_height + 1,
                                    requester_peer_id: self.peer_id.to_string(),
                                    expected_prev_hash: chain_lock.latest_hash().to_vec(),
                                    genesis_hash: chain_lock.genesis_hash().to_vec(),
                                }
                            };
                            drop(chain_lock);
                            self.broadcast_or_queue(
                                &msg,
                                &mut pending_broadcasts,
                                "pre-production sync request",
                            );
                            sync_requested = true;
                            sync_deadline =
                                Some(Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS));
                            tracing::debug!(
                                "Block production paused: verified peer tip requires sync (peer_height={}, our_height={}, same_height_divergent={})",
                                peer_height,
                                current_height,
                                same_height_divergent,
                            );
                            continue;
                        }

                        let now = chrono::Utc::now().timestamp();
                        let allowed_rank = Self::allowed_rank_for_slot(
                            next_height,
                            parent_ts,
                            node_started_unix,
                            now,
                            snapshot_size,
                        );

                        // Resolve which (if any) rank elects us. If we're not
                        // in the snapshot at all, `slot_leader_address`
                        // returns None for every rank and we don't produce.
                        let mut my_rank: Option<u32> = None;
                        for rank in 0..=allowed_rank {
                            let leader = {
                                let chain_lock = chain.lock().await;
                                chain_lock.slot_leader_address(next_height, &latest_hash, rank)
                            };
                            match leader {
                                Some(addr) if addr == my_address => {
                                    my_rank = Some(rank);
                                    break;
                                }
                                Some(_) => continue,
                                None => break,
                            }
                        }

                        if my_rank.is_none() {
                            tracing::debug!(
                                "Slot {} not ours (allowed_rank={}); waiting for elected leader",
                                next_height,
                                allowed_rank,
                            );
                            // Suppress an unused warning when the BACKUP_LEADER_TIMEOUT_SECS
                            // const is referenced in test/log compositions.
                            let _ = BACKUP_LEADER_TIMEOUT_SECS;
                            continue;
                        }

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

                                        // Emit WebSocket events: legacy `new_block` summary + `new_header`
                                        // signed-header stream consumed by light clients.
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

                                            let chain_id = {
                                                let chain_lock = chain.lock().await;
                                                chain_lock.chain_id().to_string()
                                            };
                                            let signed_header = crate::light::SignedHeader {
                                                chain_id,
                                                header: block.header.clone(),
                                                block_hash: block.hash.clone(),
                                                signature: block.signature.clone(),
                                            };
                                            let _ = etx.send(serde_json::json!({
                                                "type": "new_header",
                                                "data": signed_header,
                                            }).to_string());
                                        }

                                        // Broadcast block
                                        let msg = NetworkMessage::NewBlock(serialized);
                                        self.broadcast_or_queue(
                                            &msg,
                                            &mut pending_broadcasts,
                                            "new block",
                                        );

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
                                            self.broadcast_or_queue(
                                                &msg,
                                                &mut pending_broadcasts,
                                                "finality vote",
                                            );
                                        }

                                        seen_block_hashes.insert(block.hash);
                                    }
                                    Err(ChainError::UnauthorizedValidator)
                                    | Err(ChainError::WrongProposer { .. }) => {}
                                    Err(e) => error!("Failed to add own block: {}", e),
                                }
                            }
                            Err(ChainError::UnauthorizedValidator)
                            | Err(ChainError::WrongProposer { .. }) => {}
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
                    self.broadcast_or_queue(&msg, &mut pending_broadcasts, "height announce");
                }

                _ = rebroadcast_timer.tick() => {
                    self.flush_pending_broadcasts(&mut pending_broadcasts);
                }

                // Rate limiter + peer scoring cleanup
                _ = rate_limiter_cleanup_timer.tick() => {
                    rate_limiter.cleanup();
                    peer_scorer.decay_scores();
                    peer_scorer.cleanup();
                }

                // Network events
                event = self.swarm.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(CursBehaviourEvent::Gossipsub(
                            gossipsub::Event::Message { message, .. }
                        )) => {
                            // P2P rate limiting + peer scoring
                            if let Some(source_peer) = message.source {
                                if !rate_limiter.check(&source_peer) {
                                    peer_scorer.record_bad(&source_peer, SCORE_RATE_LIMITED);
                                    continue;
                                }
                                if peer_scorer.should_ban(&source_peer) {
                                    continue;
                                }
                            }
                            if let Ok(net_msg) = serde_json::from_slice::<NetworkMessage>(&message.data) {
                                match net_msg {
                                    NetworkMessage::NewBlock(data) => {
                                        let accepted = Self::handle_new_block(
                                            &chain,
                                            &data,
                                            &mut seen_block_hashes,
                                            &validator_key,
                                            self,
                                            &event_tx,
                                            &mut pending_broadcasts,
                                        ).await;
                                        // Score the peer based on block validity
                                        if let Some(source) = message.source {
                                            if accepted {
                                                peer_scorer.record_good(&source, SCORE_VALID_BLOCK);
                                            } else {
                                                peer_scorer.record_bad(&source, SCORE_INVALID_BLOCK);
                                            }
                                        }
                                    }
                                    NetworkMessage::NewTransaction(data) => {
                                        let tx_ok = Self::handle_new_transaction(&chain, &data, &event_tx).await;
                                        if let Some(source) = message.source {
                                            if tx_ok {
                                                peer_scorer.record_good(&source, SCORE_VALID_TX);
                                            } else {
                                                peer_scorer.record_bad(&source, SCORE_INVALID_TX);
                                            }
                                        }
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
                                            &mut pending_broadcasts,
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
                                        let our_latest_hash = chain_lock.latest_hash().to_vec();
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

                                        if verified {
                                            verified_peer_tips.insert(
                                                announce_peer_id.clone(),
                                                (height, latest_hash.clone(), Instant::now()),
                                            );
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
                                            self.broadcast_or_queue(
                                                &msg,
                                                &mut pending_broadcasts,
                                                "height-announce sync request",
                                            );
                                            sync_requested = true;
                                            sync_deadline = Some(
                                                Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS),
                                            );
                                        } else if height == our_height
                                            && latest_hash != our_latest_hash
                                            && !sync_requested
                                            && verified
                                        {
                                            warn!(
                                                "Verified peer {} has divergent tip at height {}. Requesting snapshot.",
                                                &announce_peer_id, height
                                            );
                                            let chain_lock = chain.lock().await;
                                            let msg = NetworkMessage::RequestSnapshot {
                                                requester_peer_id: self.peer_id.to_string(),
                                                preferred_height: None,
                                                start_chunk: 0,
                                                known_finalized_height: chain_lock.finalized_height(),
                                                known_finalized_hash: chain_lock
                                                    .finality_tracker
                                                    .finalized_hash
                                                    .clone(),
                                            };
                                            drop(chain_lock);
                                            self.broadcast_or_queue(
                                                &msg,
                                                &mut pending_broadcasts,
                                                "divergent-tip snapshot request",
                                            );
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
                                        if let Ok(evidence) = bounded_deserialize::<EquivocationEvidence>(&data) {
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
                                        if let Ok(vote) = bounded_deserialize::<crate::consensus::FinalityVote>(&data) {
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
                                                self.broadcast_or_queue(
                                                    &msg,
                                                    &mut pending_broadcasts,
                                                    "snapshot manifest",
                                                );
                                                for chunk in chunks.into_iter().skip(start_chunk) {
                                                    if let Ok(data) = bincode::serialize(&chunk) {
                                                        let msg = NetworkMessage::SnapshotChunk {
                                                            target_peer_id: requester_peer_id.clone(),
                                                            height: snapshot_height,
                                                            data,
                                                        };
                                                        self.broadcast_or_queue(
                                                            &msg,
                                                            &mut pending_broadcasts,
                                                            "snapshot chunk",
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    NetworkMessage::SnapshotManifest { target_peer_id, data } => {
                                        if target_peer_id != self.peer_id.to_string() {
                                            continue;
                                        }
                                        match bounded_deserialize::<SnapshotManifest>(&data) {
                                            Ok(manifest) => {
                                                // Validate the manifest before we accept it as a
                                                // pending snapshot. Without this check a single
                                                // peer can feed us a fake manifest and we will
                                                // happily start storing chunks against it (#4).
                                                let chain_lock = chain.lock().await;
                                                let our_chain_id = chain_lock.chain_id().to_string();
                                                let our_genesis = chain_lock.genesis_hash().to_vec();
                                                let our_finalized_height = chain_lock.finalized_height();
                                                let our_finalized_hash = chain_lock
                                                    .finality_tracker
                                                    .finalized_hash
                                                    .clone();
                                                drop(chain_lock);

                                                if manifest.chain_id != our_chain_id {
                                                    warn!(
                                                        "Rejecting snapshot manifest: chain_id mismatch ({} vs {})",
                                                        manifest.chain_id, our_chain_id
                                                    );
                                                    continue;
                                                }
                                                if manifest.genesis_hash != our_genesis {
                                                    warn!("Rejecting snapshot manifest: genesis hash mismatch");
                                                    continue;
                                                }
                                                if manifest.chunk_root.is_empty() {
                                                    warn!("Rejecting snapshot manifest: empty chunk_root");
                                                    continue;
                                                }
                                                if manifest.chunk_count == 0
                                                    || manifest.chunk_hashes.len() != manifest.chunk_count
                                                {
                                                    warn!(
                                                        "Rejecting snapshot manifest: chunk_count={} hashes_len={}",
                                                        manifest.chunk_count,
                                                        manifest.chunk_hashes.len()
                                                    );
                                                    continue;
                                                }
                                                if manifest.height > manifest.tip_height
                                                    && manifest.tip_height != 0
                                                {
                                                    warn!(
                                                        "Rejecting snapshot manifest: height {} > tip_height {}",
                                                        manifest.height, manifest.tip_height
                                                    );
                                                    continue;
                                                }
                                                // If we already have finality at or past the
                                                // snapshot's claimed finalized height, the hashes
                                                // must agree — otherwise the peer is pushing a
                                                // divergent history.
                                                if !our_finalized_hash.is_empty()
                                                    && our_finalized_height >= manifest.finalized_height
                                                    && our_finalized_height == manifest.finalized_height
                                                    && our_finalized_hash != manifest.finalized_hash
                                                {
                                                    warn!("Rejecting snapshot manifest: finalized hash conflicts with local finality");
                                                    continue;
                                                }

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
                                        match bounded_deserialize::<StateChunk>(&data) {
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
                                                                    self.broadcast_or_queue(
                                                                        &request,
                                                                        &mut pending_broadcasts,
                                                                        "post-snapshot block request",
                                                                    );
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
                            last_peer_change_at = Instant::now();
                            let mut state = runtime_state.write().await;
                            state.set_peer_count(self.swarm.connected_peers().count());
                            drop(state);
                            self.flush_pending_broadcasts(&mut pending_broadcasts);
                        }
                        SwarmEvent::Behaviour(CursBehaviourEvent::Mdns(
                            mdns::Event::Expired(peers)
                        )) => {
                            for (peer_id, _addr) in peers {
                                info!("Peer expired: {}", peer_id);
                                discovered_peers.remove(&peer_id);
                                verified_peer_tips.remove(&peer_id.to_string());
                                self.swarm.behaviour_mut().gossipsub.remove_explicit_peer(&peer_id);
                            }
                            last_peer_change_at = Instant::now();
                            let mut state = runtime_state.write().await;
                            state.set_peer_count(self.swarm.connected_peers().count());
                        }
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            info!("Connected to peer {}", peer_id);
                            last_peer_change_at = Instant::now();
                            let mut state = runtime_state.write().await;
                            state.set_peer_count(self.swarm.connected_peers().count());
                            drop(state);
                            self.flush_pending_broadcasts(&mut pending_broadcasts);
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            info!("Disconnected from peer {}", peer_id);
                            last_peer_change_at = Instant::now();
                            verified_peer_tips.remove(&peer_id.to_string());
                            let mut state = runtime_state.write().await;
                            state.set_peer_count(self.swarm.connected_peers().count());
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

    /// Handle a new block from the network. Returns `true` if the block was accepted.
    async fn handle_new_block(
        chain: &Arc<Mutex<Blockchain>>,
        data: &[u8],
        seen_hashes: &mut HashSet<Vec<u8>>,
        validator_key: &Option<KeyPair>,
        node: &mut Self,
        event_tx: &Option<tokio::sync::broadcast::Sender<String>>,
        pending_broadcasts: &mut VecDeque<NetworkMessage>,
    ) -> bool {
        // Dedup: hash the raw data
        let data_hash = crate::crypto::hash::sha3_hash(data);
        if seen_hashes.contains(&data_hash) {
            return false;
        }
        if seen_hashes.len() >= MAX_SEEN_BLOCKS {
            seen_hashes.clear();
        }
        seen_hashes.insert(data_hash);

        let block = match bounded_deserialize::<Block>(data) {
            Ok(b) => b,
            Err(e) => {
                warn!("Failed to deserialize block: {}", e);
                return false;
            }
        };

        let block_height = block.header.height;
        let block_hash = block.hash.clone();

        let mut chain_lock = chain.lock().await;

        // Try adding normally first, then try fork choice
        match chain_lock.add_block(block.clone()) {
            Ok(()) => {
                info!("Accepted block #{} from network", block_height);

                // Emit WebSocket events: summary + signed-header stream for light clients.
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
                    let signed_header = crate::light::SignedHeader {
                        chain_id: chain_lock.chain_id().to_string(),
                        header: block.header.clone(),
                        block_hash: block.hash.clone(),
                        signature: block.signature.clone(),
                    };
                    let _ = etx.send(
                        serde_json::json!({
                            "type": "new_header",
                            "data": signed_header,
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
                        node.broadcast_or_queue(
                            &msg,
                            pending_broadcasts,
                            "finality vote for accepted block",
                        );
                    }
                }
                return true;
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
                                    node.broadcast_or_queue(
                                        &msg,
                                        pending_broadcasts,
                                        "slashing evidence",
                                    );
                                    return false;
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
        false
    }

    async fn handle_new_transaction(
        chain: &Arc<Mutex<Blockchain>>,
        data: &[u8],
        event_tx: &Option<tokio::sync::broadcast::Sender<String>>,
    ) -> bool {
        match bounded_deserialize::<crate::core::transaction::Transaction>(data) {
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
                        true
                    }
                    Err(e) => {
                        warn!("Rejected transaction: {}", e);
                        false
                    }
                }
            }
            Err(e) => {
                warn!("Failed to deserialize transaction: {}", e);
                false
            }
        }
    }

    async fn handle_block_request(
        &mut self,
        chain: &Arc<Mutex<Blockchain>>,
        from_height: u64,
        requester_peer_id: &str,
        expected_prev_hash: &[u8],
        request_genesis_hash: &[u8],
        pending_broadcasts: &mut VecDeque<NetworkMessage>,
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
            warn!(
                "RequestBlocks from {} has checkpoint mismatch at height {}. Offering snapshot.",
                requester_peer_id,
                from_height.saturating_sub(1),
            );
            let snapshot = chain_lock.create_snapshot().ok().and_then(|manifest| {
                chain_lock
                    .get_snapshot_chunks(manifest.height)
                    .ok()
                    .map(|chunks| (manifest, chunks))
            });
            drop(chain_lock);
            if let Some((manifest, chunks)) = snapshot {
                let snapshot_height = manifest.height;
                if let Ok(data) = bincode::serialize(&manifest) {
                    let msg = NetworkMessage::SnapshotManifest {
                        target_peer_id: requester_peer_id.to_string(),
                        data,
                    };
                    self.broadcast_or_queue(
                        &msg,
                        pending_broadcasts,
                        "snapshot manifest for forked RequestBlocks",
                    );
                }
                for chunk in chunks {
                    if let Ok(data) = bincode::serialize(&chunk) {
                        let msg = NetworkMessage::SnapshotChunk {
                            target_peer_id: requester_peer_id.to_string(),
                            height: snapshot_height,
                            data,
                        };
                        self.broadcast_or_queue(
                            &msg,
                            pending_broadcasts,
                            "snapshot chunk for forked RequestBlocks",
                        );
                    }
                }
            }
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
            self.broadcast_or_queue(&msg, pending_broadcasts, "block response");
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

        // Previously this matcher rejected anything where
        // `from_height != chain.height() + 1`. That's too strict: if the
        // receiver's deadline expired and a retry was issued (now N×15s
        // later) while the original response was still in flight, the
        // *original* response arriving with a stale `from_height` was
        // silently dropped, every retry behaved the same way, and the
        // sync loop hit `Sync timed out after 3 retries`. Same shape:
        // a `NewBlock` from another peer advanced our tip during the
        // round-trip — every block in the response then deserialized
        // fine but `add_block` rejected the first one as already-known,
        // we set `accepted = 0`, never reset the deadline, and timed
        // out anyway.
        //
        // The fix: accept any response whose `from_height` is at most
        // our next expected height. We then walk through the carried
        // blocks, skip any whose height we already have, and feed the
        // rest into `add_block` in order. If the response is "ahead"
        // (`from_height > chain.height() + 1`) it can't be applied
        // contiguously, so reject it — that case is fed by the
        // RequestSnapshot path, not RequestBlocks.
        let next_expected = chain_lock.height().saturating_add(1);
        if from_height > next_expected {
            // Stale or future response; can't apply contiguously.
            return;
        }

        let mut accepted = 0u64;
        let mut skipped = 0u64;
        for data in blocks_data {
            match bounded_deserialize::<Block>(data) {
                Ok(block) => {
                    let block_height = block.header.height;
                    // Skip blocks that arrived after we already advanced
                    // past them via another path (NewBlock, prior batch).
                    if block_height <= chain_lock.height() {
                        skipped += 1;
                        continue;
                    }
                    if block_height != chain_lock.height() + 1 {
                        // Non-contiguous gap mid-batch: stop, the next
                        // request will pick up from current height.
                        warn!(
                            "Sync: non-contiguous block #{} (we expected #{}). Stopping batch.",
                            block_height,
                            chain_lock.height() + 1
                        );
                        break;
                    }
                    match chain_lock.add_block(block) {
                        Ok(()) => accepted += 1,
                        Err(e) => {
                            warn!("Sync: rejected block #{}: {}", block_height, e);
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
                "Synced {} blocks. Height: {} (skipped {} already-known)",
                accepted,
                chain_lock.height(),
                skipped,
            );
            // Reset sync state on any forward progress. If there are more
            // blocks to fetch, the next HeightAnnounce (or the deadline
            // reissue at line 521) will trigger a fresh RequestBlocks
            // anchored on the new tip.
            *sync_requested = false;
            *sync_deadline = None;
            *sync_retries = 0;
        } else if skipped as usize == blocks_data.len() && !blocks_data.is_empty() {
            // Every block in the response was already known. The remote
            // did its job — we just raced ahead. Treat as success: clear
            // the in-flight sync so the next announce can drive a fresh
            // request without paying the retry timeout.
            tracing::debug!(
                "Sync: response had {} blocks, all already known. Clearing in-flight sync.",
                skipped
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

    #[test]
    fn test_peer_scorer_good_behavior() {
        let mut scorer = PeerScorer::new();
        let peer = test_peer_id(10);
        scorer.record_good(&peer, SCORE_VALID_BLOCK);
        assert_eq!(
            scorer.get_score(&peer),
            PEER_SCORE_INITIAL + SCORE_VALID_BLOCK
        );
        assert!(!scorer.should_ban(&peer));
    }

    #[test]
    fn test_peer_scorer_bad_behavior_leads_to_ban() {
        let mut scorer = PeerScorer::new();
        let peer = test_peer_id(11);
        // Hammer with bad scores
        for _ in 0..10 {
            scorer.record_bad(&peer, SCORE_INVALID_BLOCK);
        }
        // 100 + (10 * -20) = -100, well below ban threshold (-50)
        assert!(scorer.should_ban(&peer));
    }

    #[test]
    fn test_peer_scorer_decay_toward_initial() {
        let mut scorer = PeerScorer::new();
        let peer = test_peer_id(12);
        scorer.record_good(&peer, 50);
        assert_eq!(scorer.get_score(&peer), PEER_SCORE_INITIAL + 50);
        scorer.decay_scores();
        assert_eq!(
            scorer.get_score(&peer),
            PEER_SCORE_INITIAL + 50 - PEER_SCORE_DECAY_PER_TICK
        );
    }

    #[test]
    fn test_peer_scorer_score_clamped() {
        let mut scorer = PeerScorer::new();
        let peer = test_peer_id(13);
        for _ in 0..100 {
            scorer.record_good(&peer, 50);
        }
        assert_eq!(scorer.get_score(&peer), PEER_SCORE_MAX);
    }

    /// Regression test for #3: producer used `bincode::serialize` (fixint
    /// encoding), consumer used `bincode::options()` (varint encoding by
    /// default). The mismatch corrupted nested types and surfaced as
    /// "string is not valid utf8". Now both sides MUST agree.
    #[test]
    fn test_bounded_deserialize_matches_default_serialize_for_block() {
        use crate::core::block::Block;
        let block = Block::genesis();
        let serialized = bincode::serialize(&block).expect("serialize");
        let round_tripped: Block = bounded_deserialize(&serialized)
            .expect("bounded_deserialize must accept default-bincode output");
        assert_eq!(round_tripped.hash, block.hash);
        assert_eq!(round_tripped.header.height, block.header.height);
    }

    #[test]
    fn test_bounded_deserialize_rejects_oversized_payload() {
        let huge = vec![0u8; (MAX_DESERIALIZE_SIZE + 1) as usize];
        let result: Result<Vec<u8>, String> = bounded_deserialize(&huge);
        assert!(result.is_err());
    }

    #[test]
    fn test_initial_production_gate_waits_for_peer_mesh() {
        let started = Instant::now();
        assert!(NetworkNode::should_delay_initial_production(
            started,
            0,
            0,
            started + Duration::from_secs(STARTUP_GRACE_SECS - 1),
        ));
        assert!(NetworkNode::should_delay_initial_production(
            started,
            0,
            0,
            started + Duration::from_secs(STARTUP_GRACE_SECS + 1),
        ));
        assert!(!NetworkNode::should_delay_initial_production(
            started,
            1,
            0,
            started + Duration::from_secs(STARTUP_GRACE_SECS + 1),
        ));
        assert!(!NetworkNode::should_delay_initial_production(
            started,
            1,
            1,
            started + Duration::from_secs(STARTUP_GRACE_SECS + 1),
        ));
        assert!(NetworkNode::should_delay_initial_production(
            started,
            0,
            1,
            started + Duration::from_secs(STARTUP_GRACE_SECS + 1),
        ));
        assert!(!NetworkNode::should_delay_initial_production(
            started,
            0,
            1,
            started + Duration::from_secs(PEERLESS_PRODUCTION_AFTER_SECS + 1),
        ));
    }

    #[test]
    fn test_genesis_backup_rank_uses_node_start_anchor() {
        let node_started_unix = 1_000;
        let parent_timestamp = 0;
        let now_unix = node_started_unix + 100 * BACKUP_LEADER_TIMEOUT_SECS as i64;

        assert_eq!(
            NetworkNode::allowed_rank_for_slot(1, parent_timestamp, node_started_unix, now_unix, 3,),
            0,
            "height-1 production must remain primary-only even when genesis timestamp is 0"
        );

        assert!(
            NetworkNode::allowed_rank_for_slot(2, parent_timestamp, node_started_unix, now_unix, 3)
                > 0,
            "non-genesis heights still use the parent timestamp timeout"
        );
    }

    #[test]
    fn test_pending_broadcast_queue_is_bounded() {
        let mut pending = VecDeque::new();
        for i in 0..(MAX_PENDING_BROADCASTS + 10) {
            NetworkNode::enqueue_pending_broadcast(
                &mut pending,
                NetworkMessage::HeightAnnounce {
                    height: i as u64,
                    latest_hash: vec![i as u8],
                    genesis_hash: vec![0],
                    peer_id: format!("peer-{i}"),
                    public_key: None,
                    signature: None,
                    protocol_version: 5,
                },
            );
        }
        assert_eq!(pending.len(), MAX_PENDING_BROADCASTS);
        match pending.front().expect("queued message") {
            NetworkMessage::HeightAnnounce { height, .. } => {
                assert_eq!(*height, 10);
            }
            _ => panic!("unexpected queued message"),
        }
    }

    #[test]
    fn test_verified_peer_tips_detect_higher_and_divergent_tips() {
        let mut tips = HashMap::new();
        tips.insert(
            "peer-a".to_string(),
            (
                9,
                vec![9],
                Instant::now() - Duration::from_secs(VERIFIED_TIP_TTL_SECS + 1),
            ),
        );
        tips.insert("peer-b".to_string(), (11, vec![11], Instant::now()));
        NetworkNode::prune_verified_peer_tips(&mut tips, Instant::now());
        assert!(!tips.contains_key("peer-a"));

        let needed = NetworkNode::sync_needed_from_verified_tips(&tips, 10, &[10])
            .expect("higher peer tip must trigger sync");
        assert_eq!(needed.0, 11);
        assert!(!needed.2);

        tips.clear();
        tips.insert("peer-c".to_string(), (10, vec![99], Instant::now()));
        let needed = NetworkNode::sync_needed_from_verified_tips(&tips, 10, &[10])
            .expect("same-height divergent peer tip must trigger snapshot sync");
        assert_eq!(needed.0, 10);
        assert!(needed.2);
    }

    // ─── Two-node cold-sync regression test ────────────────────────────
    //
    // Builds two real `NetworkNode`s on loopback, has node A mine N blocks,
    // then has node B (fresh, height 0) request them via the
    // `RequestBlocks` / `BlockResponse` gossipsub flow. Asserts B reaches
    // height N and every hash matches A within a short bound.
    //
    // This is the regression for the `Sync timed out, retry 1/3` symptom:
    // the receiver's deadline checked `from_height != chain.height() + 1`
    // *strictly* — if the chain's height advanced for any reason between
    // request and response (or if the response arrived with from_height
    // anchored on the original request value), the response was silently
    // dropped and the matcher kept retrying until exhausted.

    use crate::core::chain::Blockchain;
    use crate::crypto::dilithium::KeyPair;
    use libp2p::Multiaddr;

    /// Build a `NetworkNode` listening on an OS-assigned port and return
    /// the dial multiaddrs once they're known.
    async fn build_node(bootnodes: Vec<String>, topic: &str) -> (NetworkNode, Vec<Multiaddr>) {
        let identity = identity::Keypair::generate_ed25519();
        // port 0 -> let the OS pick a free port
        let mut node = NetworkNode::new(0, &bootnodes, topic, identity, &[])
            .await
            .expect("node init");
        // Drain swarm until we've observed our bound listen address.
        let mut listen_addrs: Vec<Multiaddr> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while listen_addrs.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::select! {
                event = node.swarm.select_next_some() => {
                    if let SwarmEvent::NewListenAddr { address, .. } = event {
                        // Skip non-loopback listeners (mDNS may bind multiple
                        // interfaces); we only want the loopback addr for
                        // deterministic dial.
                        let s = address.to_string();
                        if s.contains("/ip4/127.0.0.1/") || s.contains("/ip4/0.0.0.0/") {
                            listen_addrs.push(address);
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
        // Replace 0.0.0.0 with 127.0.0.1 for dialing.
        let dial_addrs: Vec<Multiaddr> = listen_addrs
            .into_iter()
            .map(|addr| {
                let s = addr.to_string().replace("/ip4/0.0.0.0/", "/ip4/127.0.0.1/");
                s.parse().unwrap_or(addr)
            })
            .collect();
        (node, dial_addrs)
    }

    /// Build a chain with `block_count` blocks produced by `validator`.
    fn build_chain_with_blocks(validator: &KeyPair, block_count: u64) -> Blockchain {
        let mut chain = Blockchain::new();
        for _ in 0..block_count {
            let block = chain
                .create_block(validator)
                .expect("create_block on test chain");
            chain.add_block(block).expect("add_block on test chain");
        }
        assert_eq!(chain.height(), block_count);
        chain
    }

    /// One iteration over all the cold-sync work: returns true if B caught up.
    async fn cold_sync_once(target_height: u64) -> bool {
        let validator = KeyPair::generate();
        let chain_a = build_chain_with_blocks(&validator, target_height);
        let chain_b = Blockchain::new();
        let chain_id = chain_a.chain_id().to_string();
        // Pin to the same protocol version both sides currently see at tip.
        let proto_a = chain_a.protocol_version_at_height(chain_a.height());
        let topic = topic_name(&chain_id, proto_a);

        // Capture A's expected per-height hashes before we move it into the Arc.
        let expected_hashes: Vec<Vec<u8>> = chain_a.blocks.iter().map(|b| b.hash.clone()).collect();
        let chain_a = Arc::new(Mutex::new(chain_a));
        let chain_b = Arc::new(Mutex::new(chain_b));

        // Bring up node A first (the source) so we can dial it from B.
        let (mut node_a, a_dial_addrs) = build_node(vec![], &topic).await;
        let bootnodes: Vec<String> = a_dial_addrs
            .iter()
            .map(|m| format!("{}/p2p/{}", m, node_a.peer_id))
            .collect();
        let (mut node_b, _b_dial) = build_node(bootnodes, &topic).await;

        // Run a bounded driver: alternate polling A and B's swarms, dispatch
        // the relevant subset of NetworkMessage variants we care about.
        // We only model NewBlock / RequestBlocks / BlockResponse here —
        // the bug is in that triplet.

        let b_peer_id = node_b.peer_id;
        let a_peer_id = node_a.peer_id;

        // Wait for the gossipsub mesh to form. Heartbeat is 10s in prod;
        // we drive both swarms until both see the other as an explicit
        // gossipsub peer (subscription event).
        let mesh_deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        let mut a_sees_b = false;
        let mut b_sees_a = false;
        while (!a_sees_b || !b_sees_a) && tokio::time::Instant::now() < mesh_deadline {
            tokio::select! {
                ev = node_a.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(CursBehaviourEvent::Gossipsub(
                        gossipsub::Event::Subscribed { peer_id, .. }
                    )) = ev
                        && peer_id == b_peer_id
                    {
                        a_sees_b = true;
                    }
                }
                ev = node_b.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(CursBehaviourEvent::Gossipsub(
                        gossipsub::Event::Subscribed { peer_id, .. }
                    )) = ev
                        && peer_id == a_peer_id
                    {
                        b_sees_a = true;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
        if !a_sees_b || !b_sees_a {
            // Mesh never formed — this is itself a bug, but we don't want
            // the test to pass silently if the prerequisite failed.
            eprintln!(
                "cold_sync_once: mesh never formed (a_sees_b={}, b_sees_a={})",
                a_sees_b, b_sees_a
            );
            return false;
        }

        // B initiates the sync. Compose a RequestBlocks message identical
        // to what `run_with_chain` emits on a verified HeightAnnounce.
        let req = {
            let chain_lock = chain_b.lock().await;
            NetworkMessage::RequestBlocks {
                from_height: chain_lock.height() + 1,
                requester_peer_id: b_peer_id.to_string(),
                expected_prev_hash: chain_lock.latest_hash().to_vec(),
                genesis_hash: chain_lock.genesis_hash().to_vec(),
            }
        };
        node_b.broadcast(&req).expect("broadcast request");

        // Drive both nodes until B reaches target_height or the bounded
        // deadline elapses. We re-issue RequestBlocks each time B sees a
        // height advance but is still below target — this models how the
        // event loop progressively pulls SYNC_BATCH_SIZE windows.
        let test_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut last_sent_from_height: u64 = 1;

        while tokio::time::Instant::now() < test_deadline {
            let height_b = chain_b.lock().await.height();
            if height_b >= target_height {
                break;
            }

            tokio::select! {
                ev = node_a.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(CursBehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { message, .. }
                    )) = ev
                        && let Ok(NetworkMessage::RequestBlocks {
                            from_height, requester_peer_id, expected_prev_hash, genesis_hash
                        }) = serde_json::from_slice::<NetworkMessage>(&message.data)
                    {
                        node_a
                            .handle_block_request(
                                &chain_a,
                                from_height,
                                &requester_peer_id,
                                &expected_prev_hash,
                                &genesis_hash,
                                &mut VecDeque::new(),
                            )
                            .await;
                    }
                }
                ev = node_b.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(CursBehaviourEvent::Gossipsub(
                        gossipsub::Event::Message { message, .. }
                    )) = ev
                        && let Ok(NetworkMessage::BlockResponse {
                            from_height, target_peer_id, responder_peer_id: _,
                            genesis_hash, blocks,
                        }) = serde_json::from_slice::<NetworkMessage>(&message.data)
                        && target_peer_id == b_peer_id.to_string()
                    {
                        let mut sync_requested = true;
                        let mut sync_deadline: Option<Instant> = None;
                        let mut sync_retries: u32 = 0;
                        NetworkNode::handle_block_response(
                            &chain_b,
                            from_height,
                            &genesis_hash,
                            &blocks,
                            &mut sync_requested,
                            &mut sync_deadline,
                            &mut sync_retries,
                        ).await;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    // Re-issue RequestBlocks if B has progressed but isn't done.
                    let height_b = chain_b.lock().await.height();
                    if height_b > 0 && height_b < target_height
                        && height_b + 1 != last_sent_from_height
                    {
                        last_sent_from_height = height_b + 1;
                        let req = {
                            let chain_lock = chain_b.lock().await;
                            NetworkMessage::RequestBlocks {
                                from_height: chain_lock.height() + 1,
                                requester_peer_id: b_peer_id.to_string(),
                                expected_prev_hash: chain_lock.latest_hash().to_vec(),
                                genesis_hash: chain_lock.genesis_hash().to_vec(),
                            }
                        };
                        let _ = node_b.broadcast(&req);
                    }
                }
            }
        }

        let final_height = chain_b.lock().await.height();
        if final_height != target_height {
            eprintln!(
                "cold_sync_once: B reached height {} (expected {})",
                final_height, target_height
            );
            return false;
        }
        // Verify every block matches A's chain.
        let chain_b_lock = chain_b.lock().await;
        for (i, expected) in expected_hashes.iter().enumerate() {
            if &chain_b_lock.blocks[i].hash != expected {
                eprintln!("cold_sync_once: hash mismatch at height {}", i);
                return false;
            }
        }
        true
    }

    /// Two-node cold sync via `RequestBlocks`/`BlockResponse`. The gap is
    /// chosen <= `SYNC_BATCH_SIZE` so we exercise the block-sync code path
    /// (not the snapshot path). Runs deterministically — invoked once per
    /// `#[tokio::test]` invocation; the per-rerun stability is asserted by
    /// the (intentional) repeated CI runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_two_node_cold_sync_via_request_blocks() {
        // 30 blocks fits in one BlockResponse (SYNC_BATCH_SIZE = 50), so a
        // single round-trip should be enough.
        assert!(
            cold_sync_once(30).await,
            "B failed to cold-sync 30 blocks from A via RequestBlocks/BlockResponse"
        );
    }

    /// Larger gap: 100 blocks requires multiple `BlockResponse` rounds
    /// (SYNC_BATCH_SIZE = 50). This is the production-realistic case
    /// quoted in the bug report ("a node that joins mid-chain") and is
    /// the path that empirically misbehaved on the live testnet.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_two_node_cold_sync_100_blocks_multi_batch() {
        assert!(
            cold_sync_once(100).await,
            "B failed to cold-sync 100 blocks across multiple BlockResponse batches"
        );
    }

    /// Regression: a `BlockResponse` whose `from_height` is older than
    /// our current tip used to be silently dropped — the strict
    /// `from_height != chain.height() + 1` matcher couldn't tell the
    /// difference between "stale retry response" and "truly mismatched
    /// response", so we kept retrying until exhaustion. After the fix,
    /// such a response is accepted: blocks below our tip are skipped,
    /// blocks at-or-above are applied contiguously, and the in-flight
    /// sync is cleared so the next announce can drive forward progress.
    #[tokio::test]
    async fn test_handle_block_response_tolerates_stale_from_height() {
        let validator = KeyPair::generate();
        // Build A first as the source of truth: 10 blocks. Each block's
        // header timestamp uses wall clock, so we *can't* re-derive A's
        // hashes by mining the same blocks on a separate chain. Instead
        // we mine on A, serialize, and replay onto B.
        let mut chain_a = Blockchain::new();
        let mut serialized_blocks = Vec::new();
        for _ in 0..10 {
            let b = chain_a.create_block(&validator).unwrap();
            serialized_blocks.push(bincode::serialize(&b).unwrap());
            chain_a.add_block(b).unwrap();
        }
        assert_eq!(chain_a.height(), 10);

        // B already advanced to height 5 via some other path (e.g. a
        // racing NewBlock gossip). Replay A's blocks 1..=5 onto B so
        // both chains are byte-identical there.
        let mut chain_b = Blockchain::new();
        for ser in serialized_blocks.iter().take(5) {
            let block = bincode::deserialize::<Block>(ser).unwrap();
            chain_b.add_block(block).unwrap();
        }
        assert_eq!(chain_b.height(), 5);
        assert_eq!(chain_a.blocks[5].hash, chain_b.blocks[5].hash);

        let chain_b = Arc::new(Mutex::new(chain_b));
        let genesis_hash = chain_b.lock().await.genesis_hash().to_vec();

        let mut sync_requested = true;
        let mut sync_deadline: Option<Instant> =
            Some(Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS));
        let mut sync_retries: u32 = 1;

        // Stale from_height (1) — pre-fix, this response was dropped
        // because `1 != chain.height() + 1 (6)`.
        NetworkNode::handle_block_response(
            &chain_b,
            1,
            &genesis_hash,
            &serialized_blocks,
            &mut sync_requested,
            &mut sync_deadline,
            &mut sync_retries,
        )
        .await;

        let height_b = chain_b.lock().await.height();
        assert_eq!(
            height_b, 10,
            "stale-from_height response must still advance the chain to the response tip"
        );
        assert!(
            !sync_requested,
            "sync should be cleared on forward progress"
        );
        assert!(sync_deadline.is_none());
        assert_eq!(sync_retries, 0);
    }

    /// Regression: a response containing only already-known blocks (the
    /// "race" case where another peer pushed `NewBlock`s while our
    /// `RequestBlocks` was in flight) used to leave `sync_deadline` set,
    /// so the next loop iteration tripped a spurious "Sync timeout,
    /// retry 1/3". After the fix, an all-skipped response clears the
    /// in-flight sync.
    #[tokio::test]
    async fn test_handle_block_response_clears_sync_on_all_skipped() {
        let validator = KeyPair::generate();
        let mut chain_b = Blockchain::new();
        for _ in 0..5 {
            let b = chain_b.create_block(&validator).unwrap();
            chain_b.add_block(b).unwrap();
        }
        let serialized: Vec<Vec<u8>> = chain_b.blocks[1..=5]
            .iter()
            .map(|b| bincode::serialize(b).unwrap())
            .collect();
        let chain_b = Arc::new(Mutex::new(chain_b));
        let genesis_hash = chain_b.lock().await.genesis_hash().to_vec();

        let mut sync_requested = true;
        let mut sync_deadline: Option<Instant> =
            Some(Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS));
        let mut sync_retries: u32 = 0;
        NetworkNode::handle_block_response(
            &chain_b,
            1,
            &genesis_hash,
            &serialized,
            &mut sync_requested,
            &mut sync_deadline,
            &mut sync_retries,
        )
        .await;
        assert!(
            !sync_requested,
            "all-skipped response must clear sync_requested"
        );
        assert!(sync_deadline.is_none());
    }

    /// Regression: a response whose `from_height` is *ahead* of our tip
    /// (i.e. would create a non-contiguous gap) must still be rejected.
    /// This case is fed by RequestSnapshot, not RequestBlocks; if we
    /// accepted it, `add_block` would later trip on `InvalidPrevHash`
    /// and the chain would be stuck.
    #[tokio::test]
    async fn test_handle_block_response_rejects_future_from_height() {
        let validator = KeyPair::generate();
        // Build A as source-of-truth, replay block 1 onto B so genesis
        // and height-1 match byte-for-byte.
        let mut chain_a = Blockchain::new();
        let mut all_serialized: Vec<Vec<u8>> = Vec::new();
        for _ in 0..7 {
            let b = chain_a.create_block(&validator).unwrap();
            all_serialized.push(bincode::serialize(&b).unwrap());
            chain_a.add_block(b).unwrap();
        }
        let mut chain_b = Blockchain::new();
        let block1: Block = bincode::deserialize(&all_serialized[0]).unwrap();
        chain_b.add_block(block1).unwrap();
        // chain_b is at height 1; chain_a is at 7.

        // Ship blocks 5..=7 only — non-contiguous from B's perspective.
        let serialized: Vec<Vec<u8>> = all_serialized[4..=6].to_vec();

        let chain_b = Arc::new(Mutex::new(chain_b));
        let genesis_hash = chain_b.lock().await.genesis_hash().to_vec();

        let mut sync_requested = true;
        let mut sync_deadline: Option<Instant> =
            Some(Instant::now() + Duration::from_secs(SYNC_TIMEOUT_SECS));
        let mut sync_retries: u32 = 0;
        NetworkNode::handle_block_response(
            &chain_b,
            5, // ahead of our height + 1 = 2
            &genesis_hash,
            &serialized,
            &mut sync_requested,
            &mut sync_deadline,
            &mut sync_retries,
        )
        .await;
        let height_b = chain_b.lock().await.height();
        assert_eq!(height_b, 1, "future-anchored response must not advance us");
        assert!(sync_requested, "sync_requested must remain set");
    }
}
