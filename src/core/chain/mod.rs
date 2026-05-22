mod apply;
mod consensus;
mod estimate;
mod finality;
mod genesis;
mod mempool;
mod persistence;
mod produce;
mod reorg;
mod replay;
mod snapshot;

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::consensus::FinalityVote;
use crate::consensus::{EpochSnapshot, EquivocationEvidence, FinalityTracker, ProofOfStake};
use crate::core::block::Block;
use crate::core::blocktree::{BlockTree, BlockTreeError};
use crate::core::receipt::{IndexedLogEntry, IndexedReceipt, LogFilter, Receipt, ReceiptLocation};
use crate::core::state_proof::{AccountProof, StorageProof};
#[cfg(test)]
use crate::core::transaction::MempoolClass;
use crate::core::transaction::Transaction;
#[cfg(test)]
use crate::core::transaction::TransactionKind;
use crate::crypto::hash;
use crate::governance::GovernanceState;
use crate::storage::{BlockBackend, InMemoryBlockBackend, Storage, StorageError};
use crate::token::TokenRegistry;
use crate::vm::VmError;
use crate::vm::state::ContractState;
use thiserror::Error;

pub const DEFAULT_BLOCK_GAS_LIMIT: u64 = 10_000_000;
pub const DEFAULT_BLOCK_REWARD: u64 = 50_000_000;
pub const DEFAULT_MIN_STAKE: u64 = 1_000_000_000;
pub const DEFAULT_UNSTAKE_DELAY_BLOCKS: u64 = 10;
pub const DEFAULT_EPOCH_LENGTH: u64 = 32;
pub const DEFAULT_JAIL_DURATION_BLOCKS: u64 = 64;
pub const DEFAULT_INITIAL_BASE_FEE_PER_GAS: u64 = 0;
pub const DEFAULT_BASE_FEE_CHANGE_DENOMINATOR: u64 = 8;
const MAX_FUTURE_BLOCK_TIME_SECS: i64 = 30;
const MAX_FUTURE_TX_TIME_SECS: i64 = 30;
const MAX_PENDING_TX_AGE_SECS: i64 = 15 * 60;
const MAX_PENDING_TRANSACTIONS: usize = 10_000;
/// Slots reserved for `MempoolClass::System` transactions (stake / unstake
/// / governance). User-class transactions cannot consume these. Sum of
/// `RESERVED_SYSTEM_SLOTS + MAX_PENDING_TRANSACTIONS_USER` equals
/// `MAX_PENDING_TRANSACTIONS`. Sized small because system traffic is rare
/// in a healthy network — the goal is starvation resistance, not
/// throughput.
const RESERVED_SYSTEM_SLOTS: usize = 500;
/// Cap on User-class mempool entries. `system_count` and `user_count` are
/// tracked independently against their respective caps in
/// `add_transaction`.
const MAX_PENDING_TRANSACTIONS_USER: usize = MAX_PENDING_TRANSACTIONS - RESERVED_SYSTEM_SLOTS;
const MAX_PENDING_TRANSACTIONS_PER_ACCOUNT: usize = 64;
const MAX_PENDING_GAS_BUDGET_MULTIPLIER: u64 = 8;
const MAX_PENDING_GAS_PER_ACCOUNT_MULTIPLIER: u64 = 2;
const MAX_PENDING_NONCE_GAP: u64 = 32;
const MIN_REPLACEMENT_FEE_BUMP_PCT: u64 = 10;
const MIN_REPLACEMENT_PRIORITY_BUMP_PCT: u64 = 25;
/// Hard cap on deployed contract bytecode (256 KB). The implicit gas-based
/// limit (block_gas_limit / GAS_PER_BYTE ≈ 625 KB) is loose; this cap gives a
/// clearer error and bounds long-term storage growth.
pub const MAX_CONTRACT_CODE_BYTES: usize = 256 * 1024;
const CHAIN_CONFIG_KEY: &[u8] = b"chain_config";
/// Bound on the small-job channel (FinalizedHeight / PendingTransactions /
/// EquivocationEvidence / Shutdown). FullState is *not* routed through this
/// channel — it goes through a single-slot latest-wins mutex so we can never
/// silently drop a snapshot/reorg/epoch persist. 8 is plenty: the producer
/// rate for these small jobs is at most a few per second under heavy traffic.
const PERSISTENCE_QUEUE_CAPACITY: usize = 8;
const PERSISTENCE_SHUTDOWN_SIGNAL_TIMEOUT: Duration = Duration::from_secs(1);
const PERSISTENCE_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Re-export of [`crate::core::state_root::V6_PROTOCOL_VERSION`] so call
/// sites inside `chain.rs` can keep using the short name without an
/// extra import. The const moved to `state_root` as part of #29.
pub use crate::core::state_root::V6_PROTOCOL_VERSION;

/// Block height at which the v6 hardfork activates on the public
/// testnet. Currently set to `u64::MAX` — the hardfork is DORMANT.
/// The full v5↔v6 dispatch is wired into every state-root call site
/// in this commit; switching the live network to v6 is a one-line
/// change to this constant + a coordinated rollout. Production
/// activation requires (1) the audit cycle to verify the SMT root
/// implementation, (2) a soak test on a localnet at v6, and (3) the
/// 3-validator testnet to coordinate a binary rollout at the same
/// pre-announced height.
pub const V6_HARDFORK_HEIGHT_TESTNET: u64 = u64::MAX;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenesisAllocation {
    pub public_key: String,
    pub balance: u64,
    pub staked_balance: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProtocolUpgrade {
    pub height: u64,
    pub version: u32,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenesisConfig {
    #[serde(default = "default_chain_id")]
    pub chain_id: String,
    #[serde(default = "default_chain_name")]
    pub chain_name: String,
    #[serde(default = "default_block_reward")]
    pub block_reward: u64,
    #[serde(default = "default_minimum_stake")]
    pub minimum_stake: u64,
    #[serde(default = "default_unstake_delay_blocks")]
    pub unstake_delay_blocks: u64,
    #[serde(default = "default_epoch_length")]
    pub epoch_length: u64,
    #[serde(default = "default_jail_duration_blocks")]
    pub jail_duration_blocks: u64,
    #[serde(default)]
    pub allocations: Vec<GenesisAllocation>,
    #[serde(default)]
    pub upgrades: Vec<ProtocolUpgrade>,
    #[serde(default = "default_block_gas_limit")]
    pub block_gas_limit: u64,
    #[serde(default = "default_initial_base_fee_per_gas")]
    pub initial_base_fee_per_gas: u64,
    #[serde(default = "default_base_fee_change_denominator")]
    pub base_fee_change_denominator: u64,
}

fn default_chain_id() -> String {
    "curs3d-devnet".to_string()
}

fn default_chain_name() -> String {
    "curs3d-devnet".to_string()
}

fn default_block_reward() -> u64 {
    DEFAULT_BLOCK_REWARD
}

fn default_minimum_stake() -> u64 {
    DEFAULT_MIN_STAKE
}

fn default_unstake_delay_blocks() -> u64 {
    DEFAULT_UNSTAKE_DELAY_BLOCKS
}

fn default_epoch_length() -> u64 {
    DEFAULT_EPOCH_LENGTH
}

fn default_jail_duration_blocks() -> u64 {
    DEFAULT_JAIL_DURATION_BLOCKS
}

fn default_block_gas_limit() -> u64 {
    DEFAULT_BLOCK_GAS_LIMIT
}

fn default_initial_base_fee_per_gas() -> u64 {
    DEFAULT_INITIAL_BASE_FEE_PER_GAS
}

fn default_base_fee_change_denominator() -> u64 {
    DEFAULT_BASE_FEE_CHANGE_DENOMINATOR
}

impl Default for GenesisConfig {
    fn default() -> Self {
        GenesisConfig {
            chain_id: "curs3d-devnet".to_string(),
            chain_name: "curs3d-devnet".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: DEFAULT_MIN_STAKE,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: Vec::new(),
            upgrades: Vec::new(),
            block_gas_limit: DEFAULT_BLOCK_GAS_LIMIT,
            initial_base_fee_per_gas: DEFAULT_INITIAL_BASE_FEE_PER_GAS,
            base_fee_change_denominator: DEFAULT_BASE_FEE_CHANGE_DENOMINATOR,
        }
    }
}

#[derive(Error, Debug)]
pub enum ChainError {
    #[error("invalid genesis config: {0}")]
    InvalidGenesis(String),
    #[error("genesis config does not match the chain already stored on disk")]
    GenesisMismatch,
    #[error("invalid block height: expected {expected}, got {got}")]
    InvalidHeight { expected: u64, got: u64 },
    #[error("invalid previous hash")]
    InvalidPrevHash,
    #[error("invalid block hash")]
    InvalidBlockHash,
    #[error("invalid merkle root")]
    InvalidMerkleRoot,
    #[error("invalid state root")]
    InvalidStateRoot,
    #[error("invalid block signature")]
    InvalidBlockSignature,
    #[error("invalid block timestamp: got {got}, expected between {min} and {max}")]
    InvalidBlockTimestamp { got: i64, min: i64, max: i64 },
    #[error("invalid transaction signature")]
    InvalidSignature,
    #[error("invalid chain id: expected {expected}, got {got}")]
    InvalidChainId { expected: String, got: String },
    #[error("invalid sender account")]
    InvalidSender,
    #[error("invalid recipient address")]
    InvalidRecipient,
    #[error("invalid transaction format: {0}")]
    InvalidTransactionFormat(&'static str),
    #[error("insufficient balance: {address} has {balance}, needs {needed}")]
    InsufficientBalance {
        address: String,
        balance: u64,
        needed: u64,
    },
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
    #[error("duplicate transaction")]
    DuplicateTransaction,
    #[error("replacement transaction fee bump too low")]
    ReplacementFeeTooLow,
    #[error("transaction fee too low for current mempool pressure")]
    FeeTooLow,
    #[error("invalid block base fee: expected {expected}, got {got}")]
    InvalidBaseFee { expected: u64, got: u64 },
    #[error("mempool full")]
    MempoolFull,
    #[error("missing coinbase transaction")]
    MissingCoinbase,
    #[error("multiple coinbase transactions")]
    MultipleCoinbase,
    #[error("invalid coinbase transaction")]
    InvalidCoinbase,
    #[error("unauthorized validator")]
    UnauthorizedValidator,
    #[error(
        "wrong proposer: expected slot-leader at rank ≤ {allowed_rank}, got non-leader for height {height}"
    )]
    WrongProposer { height: u64, allowed_rank: u32 },
    #[error("block tree error: {0}")]
    BlockTree(#[from] BlockTreeError),
    #[error("reorg blocked by finality at height {0}")]
    ReorgBelowFinality(u64),
    #[error("invalid protocol version: expected {expected}, got {got}")]
    InvalidProtocolVersion { expected: u32, got: u32 },
    #[error("gas calculation overflowed")]
    GasOverflow,
    #[error("snapshot error: {0}")]
    SnapshotError(String),
    #[error(
        "{kind} at height {height} disagrees with hardcoded checkpoint: expected {expected}, got {got}"
    )]
    CheckpointMismatch {
        height: u64,
        expected: String,
        got: String,
        kind: &'static str,
    },
    #[error("vm error: {0}")]
    VmError(#[from] VmError),
    #[error("contract not found: {0}")]
    ContractNotFound(String),
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("block store error: {0}")]
    BlockStore(#[from] crate::core::block_store::BlockStoreError),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingUnstake {
    pub amount: u64,
    pub unlock_height: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountState {
    pub balance: u64,
    pub nonce: u64,
    pub staked_balance: u64,
    pub pending_unstakes: Vec<PendingUnstake>,
    pub validator_active_from_height: u64,
    pub jailed_until_height: u64,
    pub public_key: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionEstimate {
    pub next_block_height: u64,
    pub base_fee_per_gas: u64,
    pub gas_used: u64,
    pub effective_gas_price: u64,
    pub priority_fee_paid: u64,
    pub base_fee_burned: u64,
    pub total_fee_charged: u64,
    pub gas_refunded: u64,
    pub max_total_fee: u64,
    pub would_replace_pending: bool,
}

pub struct Blockchain {
    pub accounts: HashMap<Vec<u8>, AccountState>,
    pub pending_transactions: Vec<Transaction>,
    pub block_reward: u64,
    pub minimum_stake: u64,
    pub unstake_delay_blocks: u64,
    pub epoch_length: u64,
    pub jail_duration_blocks: u64,
    pub genesis_config: GenesisConfig,
    pub block_tree: BlockTree,
    pub finality_tracker: FinalityTracker,
    pub slashed_validators: HashSet<Vec<u8>>,
    pub epoch_snapshots: HashMap<u64, EpochSnapshot>,
    pub block_gas_limit: u64,
    pub initial_base_fee_per_gas: u64,
    pub base_fee_change_denominator: u64,
    pub contracts: HashMap<Vec<u8>, ContractState>,
    pub receipts: HashMap<Vec<u8>, Receipt>,
    pub receipt_locations: HashMap<Vec<u8>, ReceiptLocation>,
    pub log_index: Vec<IndexedLogEntry>,
    pub token_registry: TokenRegistry,
    pub governance: GovernanceState,
    /// Tracks consecutive missed epochs per validator address (for inactivity penalties)
    pub validator_missed_epochs: HashMap<Vec<u8>, u64>,
    /// In-memory index from block hash to block height. Avoids O(n) scans for
    /// `block-by-hash` and `eth_getBlockByHash` lookups. Rebuilt on startup,
    /// kept in sync inside `add_block` / reorg paths.
    pub block_hash_to_height: HashMap<Vec<u8>, u64>,
    /// In-memory index from tx hash to (block_height, tx_index). Speeds up
    /// `tx/:hash`, `eth_getTransactionByHash` and any historical lookup.
    pub tx_hash_index: HashMap<Vec<u8>, (u64, usize)>,
    /// Auxiliary index for Ethereum-style EVM tx hashes — different from the
    /// CURS3D-internal `tx.hash()` used by `tx_hash_index` because the former
    /// is computed by `keccak256(rlp_signed_payload)` (what MetaMask/forge
    /// see) while the latter hashes our `bincode(Transaction)` shape. Lets
    /// `eth_getTransactionByHash` / `eth_getTransactionReceipt` answer the
    /// hashes EVM tooling actually has.
    pub evm_tx_hash_index: HashMap<Vec<u8>, (u64, usize)>,
    storage: Option<Storage>,
    persistence: PersistenceMode,
    /// Paginated block view backed by redb (in the `with_storage_mode`
    /// path) or by `InMemoryBlockBackend` (in `from_genesis`). Sole
    /// source of truth for canonical-chain block reads since #28 Phase
    /// D.3 deleted `Blockchain::blocks`. Helpers like
    /// [`Blockchain::genesis_block`], [`Blockchain::block_at_height`] and
    /// [`Blockchain::block_count`] route every read through it; writes
    /// go through [`Blockchain::push_block_internal`] /
    /// [`Blockchain::replace_all_blocks`].
    cursor: crate::core::block_store::BlockStoreCursor,
    /// Number of blocks to retain below the finalized height. `None`
    /// means archival mode (no pruning, default). `Some(N)` means each
    /// time the finality tracker finalizes a block, the cursor calls
    /// `prune_below(finalized_height - N)` to drop older blocks from
    /// both the LRU cache and the backing storage. Wired by the CLI
    /// flags `--archival` / `--prune-keep-blocks`. #28 Phase E.
    prune_keep_blocks: Option<u64>,
}

enum PersistenceMode {
    Sync,
    Async(PersistenceHandle),
}

/// Async persistence with two paths:
///   - `full_state_slot`: single-buffered, latest-wins. The producer always
///     overwrites the previous (queued-but-unprocessed) snapshot, so a
///     newer FullState supersedes an older one without ever being silently
///     dropped. Triggers `signal_sender` to wake the worker.
///   - `signal_sender` + small-job receiver: bounded channel for
///     FinalizedHeight / PendingTransactions / EquivocationEvidence and
///     the FullStateSignal/Shutdown sentinels.
///
/// Worker drains the FullState slot first (priority), then the small-job
/// channel. This keeps the chain mutex critical path non-blocking while
/// guaranteeing snapshots / reorg results / epoch persists never vanish.
///
/// The worker job loop is wrapped in `catch_unwind` so a redb panic on one
/// job logs and the worker keeps running for the next. On `Drop`, the
/// handle sends `Shutdown` and joins the thread (with a timeout) so
/// in-flight writes finish before the process exits.
struct PersistenceHandle {
    full_state_slot: Arc<StdMutex<Option<Box<PersistedChainState>>>>,
    signal_sender: SyncSender<PersistJob>,
    join_handle: StdMutex<Option<JoinHandle<()>>>,
}

struct PersistedChainState {
    genesis_config: GenesisConfig,
    finalized_height: u64,
    blocks: Vec<Block>,
    accounts: HashMap<Vec<u8>, AccountState>,
    contracts: HashMap<Vec<u8>, ContractState>,
    receipts: HashMap<Vec<u8>, Receipt>,
    epoch_snapshots: HashMap<u64, EpochSnapshot>,
    pending_transactions: Vec<Transaction>,
    token_registry: TokenRegistry,
    governance: GovernanceState,
}

enum PersistJob {
    /// Sentinel routed via the signal channel; the actual `PersistedChainState`
    /// lives in the `full_state_slot` mutex (latest-wins). Worker takes the
    /// slot's content when it dequeues this signal.
    FullStateSignal,
    PendingTransactions(Vec<Transaction>),
    FinalizedHeight(u64),
    EquivocationEvidence {
        evidence: Box<EquivocationEvidence>,
        address: Vec<u8>,
        account: Option<AccountState>,
    },
    /// Graceful shutdown: drain the FullState slot, then exit the worker loop.
    Shutdown,
}

impl PersistenceHandle {
    fn spawn(storage: Storage) -> Self {
        let (sender, receiver) = sync_channel::<PersistJob>(PERSISTENCE_QUEUE_CAPACITY);
        let full_state_slot: Arc<StdMutex<Option<Box<PersistedChainState>>>> =
            Arc::new(StdMutex::new(None));
        let worker_slot = Arc::clone(&full_state_slot);
        let join = thread::Builder::new()
            .name("curs3d-persistence".to_string())
            .spawn(move || {
                Self::run_worker(storage, receiver, worker_slot);
            })
            .expect("failed to spawn curs3d persistence worker");

        Self {
            full_state_slot,
            signal_sender: sender,
            join_handle: StdMutex::new(Some(join)),
        }
    }

    fn run_worker(
        storage: Storage,
        receiver: std::sync::mpsc::Receiver<PersistJob>,
        full_state_slot: Arc<StdMutex<Option<Box<PersistedChainState>>>>,
    ) {
        while let Ok(job) = receiver.recv() {
            // Always drain the FullState slot first when we wake up so that a
            // newer-arriving FullState from any path (epoch boundary, snapshot
            // apply, reorg) wins over staler signals still queued. This is
            // what makes the design loss-free for FullState: the slot holds
            // the latest value, and signals just nudge the worker.
            loop {
                let pending = full_state_slot
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .take();
                let Some(state) = pending else { break };
                Self::execute(
                    || Blockchain::write_full_state_to_storage(&storage, &state),
                    "full_state",
                );
            }

            match job {
                PersistJob::FullStateSignal => {
                    // Slot was already drained above; nothing more to do.
                }
                PersistJob::PendingTransactions(txs) => {
                    Self::execute(
                        || -> Result<(), StorageError> {
                            storage.replace_pending_transactions(&txs)?;
                            storage.flush()?;
                            Ok(())
                        },
                        "pending_transactions",
                    );
                }
                PersistJob::FinalizedHeight(height) => {
                    Self::execute(
                        || -> Result<(), StorageError> {
                            storage.put_meta(b"finalized_height", &height)?;
                            storage.flush()?;
                            Ok(())
                        },
                        "finalized_height",
                    );
                }
                PersistJob::EquivocationEvidence {
                    evidence,
                    address,
                    account,
                } => {
                    Self::execute(
                        || -> Result<(), StorageError> {
                            storage.put_evidence(&evidence)?;
                            if let Some(account) = account {
                                storage.put_account(&address, &account)?;
                            }
                            storage.flush()?;
                            Ok(())
                        },
                        "equivocation_evidence",
                    );
                }
                PersistJob::Shutdown => {
                    // Drain one more time in case a final FullState arrived
                    // between the loop above and this branch.
                    if let Some(state) = full_state_slot
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .take()
                    {
                        Self::execute(
                            || Blockchain::write_full_state_to_storage(&storage, &state),
                            "full_state_shutdown",
                        );
                    }
                    return;
                }
            }
        }
    }

    /// Run a persistence job under `catch_unwind` so a redb panic on one
    /// payload doesn't tear down the worker. Logs success / failure / panic
    /// distinctly so the operator can tell them apart in journald.
    fn execute<F, E>(job: F, label: &'static str)
    where
        F: FnOnce() -> Result<(), E>,
        E: std::fmt::Display,
    {
        let started = std::time::Instant::now();
        let result = catch_unwind(AssertUnwindSafe(job));
        match result {
            Ok(Ok(())) => tracing::debug!(
                target: "storage",
                event = "async_persist_ok",
                job = label,
                elapsed_ms = started.elapsed().as_millis() as u64,
            ),
            Ok(Err(err)) => tracing::error!(
                target: "storage",
                event = "async_persist_failed",
                job = label,
                error = %err,
            ),
            Err(panic) => {
                let msg = if let Some(s) = panic.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "<non-string panic payload>".to_string()
                };
                tracing::error!(
                    target: "storage",
                    event = "async_persist_panicked",
                    job = label,
                    panic = %msg,
                );
            }
        }
    }

    /// Producer-side enqueue for `FullState`: replaces the slot's contents
    /// (latest-wins) and signals the worker. Never blocks the chain mutex
    /// critical path; never silently drops state.
    fn enqueue_full_state(&self, state: Box<PersistedChainState>) {
        // 1. Replace any older queued FullState. Any prior unprocessed
        //    snapshot is now stale because this one supersedes it.
        if let Ok(mut slot) = self.full_state_slot.lock() {
            *slot = Some(state);
        }
        // 2. Wake the worker. If the small-job channel is full we don't
        //    care — the slot is set, and the worker will see it on its
        //    next dequeue (small jobs run on a tight cadence).
        let _ = self.signal_sender.try_send(PersistJob::FullStateSignal);
    }

    /// Producer-side enqueue for the small idempotent jobs. Logs and drops
    /// on full queue (each of these jobs is recreated on the next event).
    fn try_enqueue(&self, job: PersistJob, label: &'static str) {
        match self.signal_sender.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => tracing::warn!(
                target: "storage",
                event = "async_persist_skipped",
                job = label,
                reason = "queue_full",
            ),
            Err(TrySendError::Disconnected(_)) => tracing::error!(
                target: "storage",
                event = "async_persist_skipped",
                job = label,
                reason = "worker_disconnected",
            ),
        }
    }
}

impl Drop for PersistenceHandle {
    fn drop(&mut self) {
        // Best-effort graceful shutdown: signal Shutdown so the worker
        // drains the FullState slot one last time. Never block forever here:
        // shutdown must remain possible even if the persistence backend is
        // wedged inside an OS/database call.
        let mut shutdown = PersistJob::Shutdown;
        let shutdown_started = Instant::now();
        loop {
            match self.signal_sender.try_send(shutdown) {
                Ok(()) => break,
                Err(TrySendError::Full(job)) => {
                    shutdown = job;
                    if shutdown_started.elapsed() >= PERSISTENCE_SHUTDOWN_SIGNAL_TIMEOUT {
                        tracing::error!(
                            target: "storage",
                            event = "async_persist_shutdown_signal_timeout",
                            timeout_ms = PERSISTENCE_SHUTDOWN_SIGNAL_TIMEOUT.as_millis() as u64,
                        );
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
        if let Ok(mut guard) = self.join_handle.lock()
            && let Some(handle) = guard.take()
        {
            let (done_tx, done_rx) = std::sync::mpsc::channel();
            match thread::Builder::new()
                .name("curs3d-persistence-join".to_string())
                .spawn(move || {
                    let result = handle.join();
                    let _ = done_tx.send(result);
                }) {
                Ok(_) => match done_rx.recv_timeout(PERSISTENCE_JOIN_TIMEOUT) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::error!(
                            target: "storage",
                            event = "async_persist_join_failed",
                            panic = ?e,
                        );
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        tracing::error!(
                            target: "storage",
                            event = "async_persist_join_timeout",
                            timeout_ms = PERSISTENCE_JOIN_TIMEOUT.as_millis() as u64,
                        );
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        tracing::error!(
                            target: "storage",
                            event = "async_persist_join_disconnected",
                        );
                    }
                },
                Err(e) => {
                    tracing::error!(
                        target: "storage",
                        event = "async_persist_join_thread_spawn_failed",
                        error = %e,
                    );
                }
            }
        }
    }
}

impl PersistedChainState {
    fn from_chain(chain: &Blockchain) -> Self {
        // Materialize the full block sequence from the cursor. O(n) memory
        // — intrinsic to "snapshot every block for redb's replace_blocks
        // batch", which is the whole point of this struct. Hot paths
        // (header reads, fork choice) don't go through here.
        let count = chain.block_count();
        let blocks: Vec<Block> = (0..count)
            .filter_map(|h| chain.block_at_height(h))
            .collect();
        Self {
            genesis_config: chain.genesis_config.clone(),
            finalized_height: chain.finality_tracker.finalized_height,
            blocks,
            accounts: chain.accounts.clone(),
            contracts: chain.contracts.clone(),
            receipts: chain.receipts.clone(),
            epoch_snapshots: chain.epoch_snapshots.clone(),
            pending_transactions: chain.pending_transactions.clone(),
            token_registry: chain.token_registry.clone(),
            governance: chain.governance.clone(),
        }
    }
}

// `BlockExecution` moved to `chain::apply` in #29.

impl Default for Blockchain {
    fn default() -> Self {
        Self::new()
    }
}

impl Blockchain {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::from_genesis(GenesisConfig::default()).expect("default genesis must be valid")
    }

    pub fn from_genesis(genesis_config: GenesisConfig) -> Result<Self, ChainError> {
        let accounts = Self::accounts_from_genesis(&genesis_config)?;
        let contracts = HashMap::new();
        let state_root = Self::compute_state_root_full(&accounts, &contracts);
        let genesis = Block::genesis_with_state_root(
            state_root,
            &genesis_config.chain_id,
            genesis_config.initial_base_fee_per_gas,
        );
        let block_tree = BlockTree::from_genesis(&genesis);
        let mut epoch_snapshots = HashMap::new();
        epoch_snapshots.insert(
            0,
            Self::build_epoch_snapshot_for_accounts(&genesis_config, 0, &accounts, &HashSet::new()),
        );

        // Sole canonical view: an InMemoryBlockBackend seeded with the
        // genesis block, wrapped in a self-persisting cursor. The
        // self_persist flag ensures every `cursor.append` also writes the
        // block to the backend, so an LRU eviction can recover from
        // storage — required now that `self.blocks` is gone and the
        // cursor has no fallback.
        let in_mem: Arc<dyn BlockBackend> = Arc::new(InMemoryBlockBackend::new());
        in_mem.put_block(&genesis)?;
        let cursor = crate::core::block_store::BlockStoreCursor::new_self_persisting(
            in_mem,
            crate::core::block_store::DEFAULT_BLOCK_CACHE_SIZE,
        )?;

        Ok(Blockchain {
            accounts,
            pending_transactions: Vec::new(),
            block_reward: genesis_config.block_reward,
            minimum_stake: genesis_config.minimum_stake,
            unstake_delay_blocks: genesis_config.unstake_delay_blocks,
            epoch_length: genesis_config.epoch_length,
            jail_duration_blocks: genesis_config.jail_duration_blocks,
            block_gas_limit: genesis_config.block_gas_limit,
            initial_base_fee_per_gas: genesis_config.initial_base_fee_per_gas,
            base_fee_change_denominator: genesis_config.base_fee_change_denominator,
            genesis_config,
            block_tree,
            finality_tracker: FinalityTracker::new(),
            slashed_validators: HashSet::new(),
            epoch_snapshots,
            contracts,
            receipts: HashMap::new(),
            receipt_locations: HashMap::new(),
            log_index: Vec::new(),
            token_registry: TokenRegistry::new(),
            governance: GovernanceState::new(),
            validator_missed_epochs: HashMap::new(),
            block_hash_to_height: HashMap::new(),
            tx_hash_index: HashMap::new(),
            evm_tx_hash_index: HashMap::new(),
            storage: None,
            persistence: PersistenceMode::Sync,
            cursor,
            prune_keep_blocks: None,
        })
    }

    pub fn with_storage(
        data_dir: &str,
        genesis_config: Option<&GenesisConfig>,
    ) -> Result<Self, ChainError> {
        Self::with_storage_mode(data_dir, genesis_config, false)
    }

    pub fn with_storage_async_persistence(
        data_dir: &str,
        genesis_config: Option<&GenesisConfig>,
    ) -> Result<Self, ChainError> {
        Self::with_storage_mode(data_dir, genesis_config, true)
    }

    fn with_storage_mode(
        data_dir: &str,
        genesis_config: Option<&GenesisConfig>,
        async_persistence: bool,
    ) -> Result<Self, ChainError> {
        let storage = Storage::open(data_dir)?;

        if let Some(stored_height) = storage.get_height()? {
            let schema_version = storage.get_schema_version()?.unwrap_or(1);
            let stored_genesis = storage
                .get_genesis_config_compat(CHAIN_CONFIG_KEY)?
                .unwrap_or_default();

            if let Some(expected_genesis) = genesis_config
                && &stored_genesis != expected_genesis
            {
                return Err(ChainError::GenesisMismatch);
            }

            let expected_accounts = Self::accounts_from_genesis(&stored_genesis)?;
            let expected_genesis_hash = Block::genesis_with_state_root(
                Self::compute_state_root_full(&expected_accounts, &HashMap::new()),
                &stored_genesis.chain_id,
                stored_genesis.initial_base_fee_per_gas,
            )
            .hash;

            // Wire the cursor up front so we can read blocks lazily instead
            // of paging the whole chain into a `Vec<Block>` like we used to
            // pre-D.3. The cursor reads redb on demand — block_tree
            // construction still has to touch every block once, but that
            // cost is intrinsic (we can't validate a chain without seeing
            // every block) whereas the previous Vec stayed in RAM forever.
            let cursor = crate::core::block_store::BlockStoreCursor::new(
                std::sync::Arc::new(storage.clone()),
                crate::core::block_store::DEFAULT_BLOCK_CACHE_SIZE,
            )?;

            let genesis_block = cursor.block_at(0)?.ok_or(ChainError::GenesisMismatch)?;
            if genesis_block.hash != expected_genesis_hash {
                return Err(ChainError::GenesisMismatch);
            }

            let pending_transactions =
                storage.get_all_pending_transactions_compat(&stored_genesis.chain_id)?;
            let slashed_validators = storage.get_slashed_addresses()?;
            let loaded_accounts_for_weights: HashMap<Vec<u8>, AccountState> =
                storage.get_all_accounts_compat()?.into_iter().collect();

            // Rebuild block tree by walking cursor.block_at — one disk
            // read per height. Cursor's LRU cache amortizes nothing on
            // first pass but keeps a working set for later operations.
            let mut block_tree = BlockTree::from_genesis(&genesis_block);
            for h in 1..=stored_height {
                let Some(block) = cursor.block_at(h)? else {
                    break;
                };
                let proposer_stake = loaded_accounts_for_weights
                    .get(&hash::address_bytes_from_public_key(
                        &block.header.validator_public_key,
                    ))
                    .map(|a| a.staked_balance)
                    .unwrap_or(0);
                let _ = block_tree.insert(block, proposer_stake);
            }

            // Load finalized height from meta
            let finalized_height: u64 = storage.get_meta(b"finalized_height")?.unwrap_or(0);
            let finality_tracker = FinalityTracker::with_finalized(
                finalized_height,
                cursor
                    .block_at(finalized_height)?
                    .map(|b| b.hash)
                    .unwrap_or_default(),
            );

            tracing::info!(
                "Loaded blockchain from disk: chain={}, height={}, accounts={}, pending={}, slashed={}, finalized={}",
                stored_genesis.chain_name,
                stored_height,
                storage.get_all_accounts_compat()?.len(),
                pending_transactions.len(),
                slashed_validators.len(),
                finalized_height,
            );

            let stored_token_registry = storage
                .get_meta::<TokenRegistry>(crate::storage::TOKEN_REGISTRY_KEY)?
                .unwrap_or_default();
            let stored_governance = storage
                .get_meta::<GovernanceState>(crate::storage::GOVERNANCE_STATE_KEY)?
                .unwrap_or_default();

            let mut chain = Blockchain {
                accounts: HashMap::new(),
                pending_transactions,
                block_reward: stored_genesis.block_reward,
                minimum_stake: stored_genesis.minimum_stake,
                unstake_delay_blocks: stored_genesis.unstake_delay_blocks,
                epoch_length: stored_genesis.epoch_length,
                jail_duration_blocks: stored_genesis.jail_duration_blocks,
                block_gas_limit: stored_genesis.block_gas_limit,
                initial_base_fee_per_gas: stored_genesis.initial_base_fee_per_gas,
                base_fee_change_denominator: stored_genesis.base_fee_change_denominator,
                genesis_config: stored_genesis,
                block_tree,
                finality_tracker,
                slashed_validators,
                epoch_snapshots: HashMap::new(),
                contracts: HashMap::new(),
                receipts: HashMap::new(),
                receipt_locations: HashMap::new(),
                log_index: Vec::new(),
                token_registry: stored_token_registry,
                governance: stored_governance,
                validator_missed_epochs: HashMap::new(),
                block_hash_to_height: HashMap::new(),
                tx_hash_index: HashMap::new(),
                evm_tx_hash_index: HashMap::new(),
                storage: Some(storage.clone()),
                persistence: PersistenceMode::Sync,
                cursor,
                prune_keep_blocks: None,
            };

            chain.rebuild_canonical_state()?;

            if schema_version < crate::storage::CURRENT_SCHEMA_VERSION {
                chain.persist_full_state()?;
            }

            if async_persistence {
                chain.persistence = PersistenceMode::Async(PersistenceHandle::spawn(storage));
            }

            Ok(chain)
        } else {
            let mut chain = Self::from_genesis(genesis_config.cloned().unwrap_or_default())?;
            chain.storage = Some(storage.clone());
            chain.persist_full_state()?;

            // Replace the in-memory cursor produced by `from_genesis` with
            // one backed by the on-disk storage. The redb path has its own
            // persistence pipeline (`persist_added_block`), so this cursor
            // is the non-self-persisting variant — cursor.append updates
            // only the cache; storage writes happen separately.
            chain.cursor = crate::core::block_store::BlockStoreCursor::new(
                std::sync::Arc::new(storage.clone()),
                crate::core::block_store::DEFAULT_BLOCK_CACHE_SIZE,
            )?;

            if async_persistence {
                chain.persistence = PersistenceMode::Async(PersistenceHandle::spawn(storage));
            }

            tracing::info!(
                "Initialized new blockchain from genesis config: {}",
                chain.genesis_config.chain_name
            );

            Ok(chain)
        }
    }

    pub fn height(&self) -> u64 {
        self.block_count().saturating_sub(1)
    }

    /// Block at the chain tip. Returns an OWNED clone. Reads through
    /// [`Self::block_at_height`] so the cursor path is used when present.
    pub fn latest_block(&self) -> Block {
        let h = self.height();
        self.block_at_height(h)
            .expect("chain must have at least genesis")
    }

    /// Hash of the chain tip. Owned `Vec<u8>` — drops the previous
    /// `&[u8]` shape now that the underlying block storage is paginated
    /// (cursor returns owned blocks; no stable reference to lend out).
    pub fn latest_hash(&self) -> Vec<u8> {
        self.latest_block().hash
    }

    /// Hash of the genesis block. Owned `Vec<u8>` for the same reason
    /// as [`Self::latest_hash`].
    pub fn genesis_hash(&self) -> Vec<u8> {
        self.genesis_block().hash
    }

    /// Genesis block. Always present — every Blockchain is constructed with
    /// at least one block at height 0. Returns an OWNED clone.
    pub fn genesis_block(&self) -> Block {
        self.cursor
            .genesis()
            .expect("cursor genesis read must succeed (mutex poisoned)")
    }

    /// Block at a specific height, if present in the canonical chain.
    /// Returns an OWNED clone. `None` for heights above the head or
    /// below the prune watermark.
    pub fn block_at_height(&self, height: u64) -> Option<Block> {
        self.cursor
            .block_at(height)
            .expect("cursor read must succeed (storage I/O or mutex poison)")
    }

    /// Total number of blocks in the canonical chain (= head height + 1).
    pub fn block_count(&self) -> u64 {
        self.cursor
            .len()
            .expect("cursor len must succeed (mutex poisoned)")
    }

    /// Lowest block height still readable from the cursor. 0 in archival
    /// mode (default). Larger when pruning has dropped older blocks.
    /// Exposed for the `curs3d_chain_base_height` Prometheus gauge.
    pub fn chain_base_height(&self) -> u64 {
        self.cursor
            .base_height()
            .expect("cursor base_height must succeed (mutex poisoned)")
    }

    /// Enable or disable history pruning. `None` is archival (default).
    /// `Some(N)` keeps `N` blocks below the finalized height; older
    /// blocks are dropped at every finalization via `cursor.prune_below`.
    /// Wired from the `--archival` / `--prune-keep-blocks` CLI flags.
    pub fn set_prune_keep_blocks(&mut self, keep: Option<u64>) {
        self.prune_keep_blocks = keep;
    }

    /// Currently configured prune window, if any.
    pub fn prune_keep_blocks(&self) -> Option<u64> {
        self.prune_keep_blocks
    }

    // `maybe_prune_finalized` lives in `chain::finality` since #29.

    /// Iterator over every block in the canonical chain, genesis first.
    /// Yields OWNED `Block` values fetched from the cursor on demand. The
    /// iterator borrows `&self` for the duration of iteration; each step
    /// locks the cursor's mutex briefly to read one block. Memory cost is
    /// O(1) per step (no chain-length Vec materialized).
    pub fn iter_blocks(&self) -> impl DoubleEndedIterator<Item = Block> + '_ {
        let count = self.block_count();
        (0..count).filter_map(move |h| self.block_at_height(h))
    }

    /// Append a block to the canonical chain via the cursor. Caller must
    /// have validated `block.header.height == self.block_count()` upstream.
    /// In the storage-less path (`from_genesis`) the cursor is in
    /// self-persisting mode so `append` also writes to the in-memory
    /// backend; in the redb path persistence happens separately via
    /// `persist_added_block` and the cursor's append only refreshes its
    /// cache.
    fn push_block_internal(&mut self, block: Block) {
        let height = block.header.height;
        if let Err(e) = self.cursor.append(block) {
            // Append failures here are fatal-class: either the cursor's
            // mutex is poisoned, or the in-memory backend's put_block
            // failed (also mutex poison). Logging-and-continue keeps the
            // node alive long enough for the operator to notice the next
            // height mismatch — there is no longer a `self.blocks`
            // fallback to mask the issue.
            tracing::error!(
                height = height,
                error = %e,
                "BlockStoreCursor.append failed in push_block_internal — chain head is now inconsistent"
            );
        }
    }

    /// Replace the entire canonical chain with a new sequence. Used by
    /// `apply_snapshot` and `replace_blocks` (reorg). Truncates the cursor
    /// to height 0 and re-feeds it the new block sequence.
    fn replace_all_blocks(&mut self, blocks: Vec<Block>) {
        if let Err(e) = self.cursor.invalidate_from(0) {
            tracing::warn!(error = %e, "cursor.invalidate_from(0) failed during chain replace");
        }
        for block in blocks {
            let height = block.header.height;
            if let Err(e) = self.cursor.append(block) {
                tracing::error!(
                    height = height,
                    error = %e,
                    "cursor.append failed during chain replace"
                );
            }
        }
    }

    pub fn chain_id(&self) -> &str {
        &self.genesis_config.chain_id
    }

    #[allow(dead_code)]
    pub fn current_base_fee_per_gas(&self) -> u64 {
        self.latest_block().header.base_fee_per_gas
    }

    fn target_block_gas_usage(&self) -> u64 {
        (self.block_gas_limit / 2).max(1)
    }

    pub fn next_base_fee_per_gas(&self, parent: &Block) -> u64 {
        let parent_base_fee = if parent.header.height == 0 {
            parent
                .header
                .base_fee_per_gas
                .max(self.initial_base_fee_per_gas)
        } else {
            parent.header.base_fee_per_gas
        };
        let target = self.target_block_gas_usage();
        // Transition: if parent had base_fee=0 (legacy), start charging from 1
        if parent_base_fee == 0 {
            return 1;
        }
        if parent.header.gas_used == target {
            return parent_base_fee;
        }

        let delta = parent.header.gas_used.abs_diff(target);
        let change = parent_base_fee
            .max(1)
            .saturating_mul(delta)
            .checked_div(target.saturating_mul(self.base_fee_change_denominator.max(1)))
            .unwrap_or(0)
            .max(1);

        if parent.header.gas_used > target {
            parent_base_fee.saturating_add(change)
        } else {
            parent_base_fee.saturating_sub(change).max(1)
        }
    }

    pub fn current_epoch(&self) -> u64 {
        self.height() / self.epoch_length.max(1)
    }

    pub fn current_epoch_start_height(&self) -> u64 {
        self.current_epoch() * self.epoch_length.max(1)
    }

    pub fn epoch_for_height(&self, height: u64) -> u64 {
        height / self.epoch_length.max(1)
    }

    // Epoch snapshots, validator-set, protocol-version dispatch, and
    // backup-rank timing live in `chain::consensus` since #29.

    // Snapshot generation + apply lives in `chain::snapshot` since #29.

    #[allow(dead_code)]
    pub fn get_balance(&self, address: &[u8]) -> u64 {
        self.accounts.get(address).map(|a| a.balance).unwrap_or(0)
    }

    pub fn get_staked_balance(&self, address: &[u8]) -> u64 {
        self.accounts
            .get(address)
            .map(|a| a.staked_balance)
            .unwrap_or(0)
    }

    pub fn get_account(&self, address: &[u8]) -> AccountState {
        self.accounts.get(address).cloned().unwrap_or_default()
    }

    pub fn get_receipt(&self, tx_hash: &[u8]) -> Option<IndexedReceipt> {
        let receipt = self.receipts.get(tx_hash)?.clone();
        let location = self.receipt_locations.get(tx_hash)?.clone();
        Some(IndexedReceipt {
            tx_hash: tx_hash.to_vec(),
            block_height: location.block_height,
            tx_index: location.tx_index,
            receipt,
        })
    }

    pub fn query_logs(&self, filter: &LogFilter) -> Vec<IndexedLogEntry> {
        let limit = filter.limit.unwrap_or(100).min(1000);
        self.log_index
            .iter()
            .filter(|entry| {
                filter
                    .contract
                    .as_ref()
                    .is_none_or(|contract| &entry.contract == contract)
                    && filter
                        .topic
                        .as_ref()
                        .is_none_or(|topic| entry.topics.iter().any(|candidate| candidate == topic))
                    && filter.topics.as_ref().is_none_or(|positional| {
                        positional.iter().enumerate().all(|(i, expected)| {
                            expected.as_ref().is_none_or(|wanted| {
                                entry.topics.get(i).is_some_and(|actual| actual == wanted)
                            })
                        })
                    })
                    && filter
                        .from_block
                        .is_none_or(|from_block| entry.block_height >= from_block)
                    && filter
                        .to_block
                        .is_none_or(|to_block| entry.block_height <= to_block)
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Return all transactions involving `address` (as sender or recipient).
    /// Walks the chain newest-first and stops at `limit` results.
    pub fn transactions_for_address(
        &self,
        address: &[u8],
        from_block: Option<u64>,
        to_block: Option<u64>,
        limit: usize,
    ) -> Vec<(u64, usize, Transaction)> {
        let limit = limit.min(1000);
        let mut out = Vec::new();
        for block in self.iter_blocks().rev() {
            if let Some(to) = to_block
                && block.header.height > to
            {
                continue;
            }
            if let Some(from) = from_block
                && block.header.height < from
            {
                break;
            }
            for (idx, tx) in block.transactions.iter().enumerate() {
                if tx.from == address || tx.to == address {
                    out.push((block.header.height, idx, tx.clone()));
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

    // `estimate_transaction` lives in `chain::estimate` since #29.

    fn rebuild_receipt_indexes(&mut self) {
        self.receipt_locations.clear();
        self.log_index.clear();

        // Walk via cursor block-by-block. Each iteration materializes one
        // owned `Block` on the stack — O(1) memory per step — which the
        // borrow checker is happy with because nothing borrows
        // `&self.blocks` anymore. The cursor's LRU cache is reused
        // afterwards by validate / fork-choice code.
        let count = self.block_count();
        for h in 0..count {
            let Some(block) = self.block_at_height(h) else {
                continue;
            };
            for (tx_index, tx) in block.transactions.iter().enumerate() {
                if tx.is_coinbase() {
                    continue;
                }
                let tx_hash = tx.hash();
                let Some(receipt) = self.receipts.get(&tx_hash) else {
                    continue;
                };
                self.receipt_locations.insert(
                    tx_hash.clone(),
                    ReceiptLocation {
                        block_height: block.header.height,
                        tx_index,
                    },
                );
                for (log_index, log) in receipt.logs.iter().enumerate() {
                    self.log_index.push(IndexedLogEntry {
                        block_height: block.header.height,
                        tx_index,
                        log_index,
                        tx_hash: tx_hash.clone(),
                        contract: log.contract.clone(),
                        topics: log.topics.clone(),
                        data: log.data.clone(),
                    });
                }
            }
        }
        self.log_index.sort_by(|a, b| {
            a.block_height
                .cmp(&b.block_height)
                .then_with(|| a.tx_index.cmp(&b.tx_index))
                .then_with(|| a.log_index.cmp(&b.log_index))
        });
    }

    // `add_transaction` lives in `chain::mempool` since #29.
    // `create_block` lives in `chain::produce` since #29.

    // Finality votes + equivocation slashing live in
    // `chain::finality` since #29.

    /// Count of validators that have ever been slashed (equivocation, etc.).
    /// Used by /api/metrics.
    pub fn slashed_validator_count(&self) -> usize {
        self.slashed_validators.len()
    }

    /// Count of currently-jailed validators at the current head height.
    pub fn jailed_validator_count(&self) -> usize {
        let head = self.height();
        self.accounts
            .values()
            .filter(|a| a.jailed_until_height > head)
            .count()
    }

    pub fn active_validator_count(&self) -> usize {
        ProofOfStake::with_slashed(
            self.minimum_stake,
            self.slashed_validators.clone(),
            self.height() + 1,
        )
        .active_validators(&self.accounts)
        .len()
    }

    #[allow(dead_code)]
    pub fn is_valid(&self) -> bool {
        let mut replay = match Self::from_genesis(self.genesis_config.clone()) {
            Ok(chain) => chain,
            Err(_) => return false,
        };

        for block in self.iter_blocks().skip(1) {
            if replay.add_block(block.clone()).is_err() {
                return false;
            }
        }

        replay.accounts == self.accounts
            && replay.contracts == self.contracts
            && replay.token_registry == self.token_registry
    }

    // Fork-choice (`add_block_with_fork_choice` + `reorg_to_canonical_tip`)
    // lives in `chain::reorg` since #29.

    // State-root computation lives in `crate::core::state_root` since #29
    // sibling-extraction. These three methods are thin delegators kept on
    // `Blockchain` so existing call sites (`Blockchain::compute_state_root_*`)
    // continue to compile unchanged.

    #[allow(dead_code)]
    pub fn compute_state_root(accounts: &HashMap<Vec<u8>, AccountState>) -> Vec<u8> {
        crate::core::state_root::compute(accounts)
    }

    pub fn compute_state_root_full(
        accounts: &HashMap<Vec<u8>, AccountState>,
        contracts: &HashMap<Vec<u8>, ContractState>,
    ) -> Vec<u8> {
        crate::core::state_root::compute_full(accounts, contracts)
    }

    pub fn compute_state_root_at_protocol(
        accounts: &HashMap<Vec<u8>, AccountState>,
        contracts: &HashMap<Vec<u8>, ContractState>,
        protocol_version: u32,
    ) -> Vec<u8> {
        crate::core::state_root::compute_at_protocol(accounts, contracts, protocol_version)
    }

    // `accounts_from_genesis`, `decode_public_key_hex`, and
    // `next_epoch_start_height_for` live in `chain::genesis` since #29.

    // State-proof generation + verification lives in
    // `crate::core::state_proof` since #29. These wrappers delegate so
    // existing callers (HTTP API endpoints, tests) compile unchanged.

    pub fn get_account_proof(&self, address: &[u8]) -> Option<AccountProof> {
        crate::core::state_proof::generate_account_proof(address, &self.accounts, &self.contracts)
    }

    #[allow(dead_code)]
    pub fn verify_account_proof(proof: &AccountProof) -> bool {
        crate::core::state_proof::verify_account_proof(proof)
    }

    pub fn get_storage_proof(&self, contract_address: &[u8], key: &[u8]) -> Option<StorageProof> {
        crate::core::state_proof::generate_storage_proof(
            contract_address,
            key,
            &self.accounts,
            &self.contracts,
        )
    }

    #[allow(dead_code)]
    pub fn verify_storage_proof(proof: &StorageProof) -> bool {
        crate::core::state_proof::verify_storage_proof(proof)
    }

    // `snapshot_for_height`, `ensure_validator_is_authorized_for_accounts_at_rank`,
    // `proposer_addresses_for_settling_epoch`, `apply_epoch_settlement_for_block`,
    // `slot_leader_address` live in `chain::consensus` since #29.

    // replay_state_to_tip + replay_state_to_canonical_height live
    // in `chain::replay` since #29.

    // `ensure_transaction_fee_covers_base` + `priority_fee_for_transaction`
    // live in `chain::apply` since #29.

    // `apply_user_transaction` + `apply_coinbase_transaction` live in
    // `chain::apply` since #29.

    // `apply_unstake_unlocks` lives in `chain::apply` since #29.

    fn remove_block_transactions_from_mempool(&mut self, block: &Block) {
        let included_hashes: HashSet<Vec<u8>> = block
            .transactions
            .iter()
            .filter(|tx| !tx.is_coinbase())
            .map(Transaction::hash)
            .collect();

        self.pending_transactions
            .retain(|pending| !included_hashes.contains(&pending.hash()));
    }

    // Mempool admission control, sorting, eviction, fee floors:
    // see `crate::core::chain::mempool`.

    // rebuild_canonical_state lives in `chain::replay` since #29.
}

#[cfg(test)]
mod tests;
