mod apply;
mod consensus;
mod finality;
mod mempool;
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

    pub fn estimate_transaction(
        &self,
        tx: &Transaction,
    ) -> Result<TransactionEstimate, ChainError> {
        if tx.is_coinbase() {
            return Err(ChainError::InvalidTransactionFormat(
                "coinbase transactions cannot be estimated",
            ));
        }
        if tx.chain_id != self.genesis_config.chain_id {
            return Err(ChainError::InvalidChainId {
                expected: self.genesis_config.chain_id.clone(),
                got: tx.chain_id.clone(),
            });
        }

        let pending_base_fee = self.next_base_fee_per_gas(&self.latest_block());
        let replacement_index = self
            .pending_transactions
            .iter()
            .position(|pending| pending.from == tx.from && pending.nonce == tx.nonce);

        let mut projected_accounts = self.accounts.clone();
        let mut projected_contracts = self.contracts.clone();
        let mut projected_receipts = HashMap::new();
        let mut projected_token_registry = self.token_registry.clone();
        let mut projected_governance = self.governance.clone();
        let mut seen_hashes = HashSet::new();

        for (index, pending) in self.pending_transactions.iter().enumerate() {
            if replacement_index == Some(index) {
                continue;
            }
            let pending_hash = pending.hash();
            if !seen_hashes.insert(pending_hash) {
                continue;
            }
            Self::apply_user_transaction(
                &mut projected_accounts,
                &mut projected_contracts,
                &mut projected_receipts,
                &mut projected_token_registry,
                &mut projected_governance,
                pending,
                self.height() + 1,
                self.unstake_delay_blocks,
                self.epoch_length,
                self.minimum_stake,
                pending_base_fee,
            )?;
        }

        let gas_used = Self::apply_user_transaction(
            &mut projected_accounts,
            &mut projected_contracts,
            &mut projected_receipts,
            &mut projected_token_registry,
            &mut projected_governance,
            tx,
            self.height() + 1,
            self.unstake_delay_blocks,
            self.epoch_length,
            self.minimum_stake,
            pending_base_fee,
        )?;
        let effective_gas_price = tx
            .effective_gas_price(pending_base_fee)
            .ok_or(ChainError::FeeTooLow)?;
        let total_fee_charged = gas_used.saturating_mul(effective_gas_price);
        let gas_refunded = tx.total_fee_cap().saturating_sub(total_fee_charged);
        let priority_fee_paid = Self::priority_fee_for_transaction(tx, gas_used, pending_base_fee);
        let base_fee_burned = gas_used.saturating_mul(pending_base_fee);

        Ok(TransactionEstimate {
            next_block_height: self.height() + 1,
            base_fee_per_gas: pending_base_fee,
            gas_used,
            effective_gas_price,
            priority_fee_paid,
            base_fee_burned,
            total_fee_charged,
            gas_refunded,
            max_total_fee: tx.total_fee_cap(),
            would_replace_pending: replacement_index.is_some(),
        })
    }

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

    fn persist_full_state(&self) -> Result<(), ChainError> {
        if self.storage.is_none() {
            return Ok(());
        }

        let started = std::time::Instant::now();
        let state = PersistedChainState::from_chain(self);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        // Surface clone latency: this work happens under chain.lock(), so a
        // slow clone directly translates into consensus / RPC / gossipsub
        // jitter. Above 200ms is a yellow flag, above 500ms is red.
        if elapsed_ms > 200 {
            tracing::warn!(
                target: "storage",
                event = "persisted_state_clone_slow",
                elapsed_ms,
                blocks = state.blocks.len(),
                accounts = state.accounts.len(),
                contracts = state.contracts.len(),
            );
        } else {
            tracing::debug!(
                target: "storage",
                event = "persisted_state_clone",
                elapsed_ms,
                blocks = state.blocks.len(),
            );
        }

        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    Self::write_full_state_to_storage(storage, &state)?;
                }
            }
            PersistenceMode::Async(handle) => {
                handle.enqueue_full_state(Box::new(state));
            }
        }
        Ok(())
    }

    fn write_full_state_to_storage(
        storage: &Storage,
        state: &PersistedChainState,
    ) -> Result<(), ChainError> {
        storage.put_meta(CHAIN_CONFIG_KEY, &state.genesis_config)?;
        storage.put_meta(b"finalized_height", &state.finalized_height)?;
        storage.put_meta(
            crate::storage::SCHEMA_VERSION_KEY,
            &crate::storage::CURRENT_SCHEMA_VERSION,
        )?;
        storage.put_meta(crate::storage::TOKEN_REGISTRY_KEY, &state.token_registry)?;
        storage.put_meta(crate::storage::GOVERNANCE_STATE_KEY, &state.governance)?;
        storage.replace_blocks(&state.blocks)?;
        storage.replace_accounts(&state.accounts)?;
        storage.replace_contracts(&state.contracts)?;
        storage.replace_receipts(&state.receipts)?;
        storage.replace_epoch_snapshots(&state.epoch_snapshots)?;
        storage.replace_pending_transactions(&state.pending_transactions)?;
        storage.flush()?;
        Ok(())
    }

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

    fn accounts_from_genesis(
        genesis_config: &GenesisConfig,
    ) -> Result<HashMap<Vec<u8>, AccountState>, ChainError> {
        let mut accounts = HashMap::new();

        for allocation in &genesis_config.allocations {
            let public_key = Self::decode_public_key_hex(&allocation.public_key)?;
            let address = hash::address_bytes_from_public_key(&public_key);
            if accounts.contains_key(&address) {
                return Err(ChainError::InvalidGenesis(
                    "duplicate account in genesis".to_string(),
                ));
            }

            accounts.insert(
                address,
                AccountState {
                    balance: allocation.balance,
                    nonce: 0,
                    staked_balance: allocation.staked_balance,
                    pending_unstakes: Vec::new(),
                    validator_active_from_height: if allocation.staked_balance
                        >= genesis_config.minimum_stake
                    {
                        1
                    } else {
                        0
                    },
                    jailed_until_height: 0,
                    public_key: Some(public_key),
                },
            );
        }

        Ok(accounts)
    }

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

    fn decode_public_key_hex(value: &str) -> Result<Vec<u8>, ChainError> {
        let raw = value.strip_prefix("0x").unwrap_or(value);
        hex::decode(raw).map_err(|_| {
            ChainError::InvalidGenesis("invalid public_key hex in genesis".to_string())
        })
    }

    fn next_epoch_start_height_for(current_height: u64, epoch_length: u64) -> u64 {
        let epoch_length = epoch_length.max(1);
        current_height
            .saturating_div(epoch_length)
            .saturating_add(1)
            .saturating_mul(epoch_length)
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

    fn persist_pending_transactions(&self) -> Result<(), ChainError> {
        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    storage.replace_pending_transactions(&self.pending_transactions)?;
                    storage.flush()?;
                }
            }
            PersistenceMode::Async(handle) => {
                handle.try_enqueue(
                    PersistJob::PendingTransactions(self.pending_transactions.clone()),
                    "pending_transactions",
                );
            }
        }
        Ok(())
    }

    fn persist_added_block(&self, block: &Block) -> Result<(), ChainError> {
        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    storage.put_block(block)?;
                }
            }
            PersistenceMode::Async(_) => {
                // Full state snapshots at epoch boundaries persist blocks,
                // state, receipts, and pending txs together. Avoiding one
                // sled write per block is the point of async live mode.
            }
        }
        Ok(())
    }

    fn persist_finalized_height(&self, height: u64) {
        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    let _ = storage.put_meta(b"finalized_height", &height);
                    let _ = storage.flush();
                }
            }
            PersistenceMode::Async(handle) => {
                handle.try_enqueue(PersistJob::FinalizedHeight(height), "finalized_height");
            }
        }
    }

    fn persist_equivocation(&self, evidence: &EquivocationEvidence) {
        let address = hash::address_bytes_from_public_key(&evidence.validator_public_key);
        let account = self.accounts.get(&address).cloned();
        match &self.persistence {
            PersistenceMode::Sync => {
                if let Some(ref storage) = self.storage {
                    let _ = storage.put_evidence(evidence);
                    if let Some(ref account) = account {
                        let _ = storage.put_account(&address, account);
                    }
                    let _ = storage.flush();
                }
            }
            PersistenceMode::Async(handle) => {
                handle.try_enqueue(
                    PersistJob::EquivocationEvidence {
                        evidence: Box::new(evidence.clone()),
                        address,
                        account,
                    },
                    "equivocation_evidence",
                );
            }
        }
    }

    // rebuild_canonical_state lives in `chain::replay` since #29.
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::dilithium::KeyPair;

    #[test]
    fn test_new_blockchain() {
        let chain = Blockchain::new();
        assert_eq!(chain.height(), 0);
        assert!(chain.is_valid());
    }

    #[test]
    fn test_custom_genesis_activates_validator() {
        let validator = KeyPair::generate();
        let chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "curs3d-test".to_string(),
            chain_name: "curs3d-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 100,
                staked_balance: 5_000,
            }],
            ..Default::default()
        })
        .unwrap();

        assert_eq!(chain.active_validator_count(), 1);
        assert_eq!(chain.genesis_config.chain_name, "curs3d-test");
    }

    #[test]
    fn test_create_and_add_block() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
        assert_eq!(chain.height(), 1);
        assert!(chain.is_valid());
    }

    #[test]
    fn test_base_fee_rises_after_busy_block() {
        let validator = KeyPair::generate();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "base-fee-test".to_string(),
            chain_name: "base-fee-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            block_gas_limit: 100_000,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        })
        .unwrap();

        let wasm_code = br#"(module
            (memory (export "memory") 1)
            (func (export "curs3d_call"))
        )"#
        .to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            80_000,
            0,
            0,
        )
        .with_fee_caps(1_000, 100);
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        // Genesis base_fee=0 for backwards compat, transitions to >=1 on first block
        let initial_fee = chain.current_base_fee_per_gas();
        assert_eq!(initial_fee, 0); // Genesis starts at 0
        let block1 = chain.create_block(&validator).unwrap();
        assert!(block1.header.gas_used > chain.target_block_gas_usage());
        chain.add_block(block1).unwrap();

        let block2 = chain.create_block(&validator).unwrap();
        assert!(block2.header.base_fee_per_gas > initial_fee);
    }

    #[test]
    fn test_transaction_flow() {
        let mut chain = Blockchain::new();
        let validator_kp = KeyPair::generate();
        let recipient = KeyPair::generate();

        let block = chain.create_block(&validator_kp).unwrap();
        chain.add_block(block).unwrap();

        let sender_address = hash::address_bytes_from_public_key(&validator_kp.public_key);
        let recipient_address = hash::address_bytes_from_public_key(&recipient.public_key);
        let mut tx = Transaction::new(
            chain.chain_id(),
            validator_kp.public_key.clone(),
            recipient_address.clone(),
            1000,
            10,
            0,
        );
        tx.sign(&validator_kp);
        chain.add_transaction(tx).unwrap();

        let block = chain.create_block(&validator_kp).unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(chain.get_balance(&recipient_address), 1000);
        // Sender paid: 1000 transfer + gas fees (base_fee >= 1)
        let sender_balance = chain.get_balance(&sender_address);
        assert!(sender_balance < DEFAULT_BLOCK_REWARD * 2 - 1000);
        assert!(sender_balance > DEFAULT_BLOCK_REWARD * 2 - 1000 - 100_000); // Reasonable fee range
        assert!(chain.is_valid());
    }

    #[test]
    fn test_rejects_forged_mint_transaction() {
        let mut chain = Blockchain::new();
        let attacker = KeyPair::generate();
        let victim = KeyPair::generate();

        let mut tx = Transaction::new(
            chain.chain_id(),
            attacker.public_key.clone(),
            hash::address_bytes_from_public_key(&victim.public_key),
            1_000,
            10,
            0,
        );
        tx.sign(&attacker);

        let err = chain.add_transaction(tx).unwrap_err();
        assert!(matches!(err, ChainError::InsufficientBalance { .. }));
    }

    #[test]
    fn test_stake_locks_funds() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let address = hash::address_bytes_from_public_key(&validator.public_key);

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let mut stake_tx = Transaction::stake(
            chain.chain_id(),
            validator.public_key.clone(),
            10_000_000,
            5,
            0,
        );
        stake_tx.sign(&validator);
        chain.add_transaction(stake_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(chain.get_staked_balance(&address), 10_000_000);
        // Balance = 2 block rewards - staked - gas fees
        let balance = chain.get_balance(&address);
        assert!(balance < DEFAULT_BLOCK_REWARD * 2 - 10_000_000);
        assert!(balance > DEFAULT_BLOCK_REWARD * 2 - 10_000_000 - 100_000);
    }

    #[test]
    fn test_rejects_duplicate_pending_transaction() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let mut tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            hash::address_bytes_from_public_key(&recipient.public_key),
            1000,
            10,
            0,
        );
        tx.sign(&validator);

        chain.add_transaction(tx.clone()).unwrap();
        let err = chain.add_transaction(tx).unwrap_err();
        assert!(matches!(err, ChainError::DuplicateTransaction));
    }

    #[test]
    fn test_mempool_evicts_low_fee_under_gas_pressure() {
        let keypairs: Vec<KeyPair> = (0..9).map(|_| KeyPair::generate()).collect();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "mempool-pressure-test".to_string(),
            chain_name: "mempool-pressure-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            block_gas_limit: 100_000,
            allocations: keypairs
                .iter()
                .map(|kp| GenesisAllocation {
                    public_key: hex::encode(&kp.public_key),
                    balance: 10_000_000,
                    staked_balance: 0,
                })
                .collect(),
            ..Default::default()
        })
        .unwrap();

        let wasm_code =
            br#"(module (memory (export "memory") 1) (func (export "curs3d_call")))"#.to_vec();
        for kp in keypairs.iter().take(8) {
            let mut tx = Transaction::deploy_contract(
                chain.chain_id(),
                kp.public_key.clone(),
                wasm_code.clone(),
                100_000,
                0,
                0,
            )
            .with_fee_caps(20, 1);
            tx.sign(kp);
            chain.add_transaction(tx).unwrap();
        }

        let premium = keypairs.last().unwrap();
        let mut tx3 = Transaction::deploy_contract(
            chain.chain_id(),
            premium.public_key.clone(),
            wasm_code,
            100_000,
            0,
            0,
        )
        .with_fee_caps(20, 10);
        tx3.sign(premium);
        chain.add_transaction(tx3.clone()).unwrap();

        assert!(!chain.pending_transactions.is_empty());
        assert!(chain.pending_transactions.len() < 9);
        assert!(
            chain
                .pending_transactions
                .iter()
                .any(|pending| pending.hash() == tx3.hash())
        );
        assert!(chain.pending_gas_usage() <= chain.pending_gas_budget());
    }

    #[test]
    fn test_replacement_requires_priority_fee_bump() {
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "replacement-fee-test".to_string(),
            chain_name: "replacement-fee-test".to_string(),
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 50_000_000,
                staked_balance: 0,
            }],
            ..Default::default()
        })
        .unwrap();

        let recipient_address = hash::address_bytes_from_public_key(&recipient.public_key);
        let mut tx1 = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            recipient_address.clone(),
            1_000,
            0,
            0,
        )
        .with_fee_caps(10, 2);
        tx1.sign(&validator);
        chain.add_transaction(tx1).unwrap();

        let mut replacement = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            recipient_address,
            2_000,
            0,
            0,
        )
        .with_fee_caps(20, 2);
        replacement.sign(&validator);
        let err = chain.add_transaction(replacement).unwrap_err();
        assert!(matches!(err, ChainError::ReplacementFeeTooLow));
    }

    #[test]
    fn test_estimate_transaction_reports_fee_breakdown() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let mut tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            hash::address_bytes_from_public_key(&recipient.public_key),
            1_000,
            0,
            0,
        )
        .with_fee_caps(10, 2);
        tx.sign(&validator);

        let estimate = chain.estimate_transaction(&tx).unwrap();
        assert_eq!(estimate.next_block_height, chain.height() + 1);
        assert!(estimate.gas_used > 0);
        assert!(estimate.total_fee_charged > 0);
        assert_eq!(
            estimate.priority_fee_paid + estimate.base_fee_burned + estimate.gas_refunded,
            estimate.max_total_fee
        );
    }

    #[test]
    fn test_rejects_excessive_pending_nonce_gap() {
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "nonce-gap-test".to_string(),
            chain_name: "nonce-gap-test".to_string(),
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 50_000_000,
                staked_balance: 0,
            }],
            ..Default::default()
        })
        .unwrap();

        let mut tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            hash::address_bytes_from_public_key(&recipient.public_key),
            1_000,
            0,
            MAX_PENDING_NONCE_GAP + 1,
        )
        .with_fee_caps(10, 2);
        tx.sign(&validator);
        let err = chain.add_transaction(tx).unwrap_err();
        assert!(matches!(err, ChainError::InvalidTransactionFormat(_)));
    }

    #[test]
    fn test_unstake_unlocks_funds() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        let address = hash::address_bytes_from_public_key(&validator.public_key);

        // Mine a block to get funds
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Stake 10M
        let mut stake_tx = Transaction::stake(
            chain.chain_id(),
            validator.public_key.clone(),
            10_000_000,
            5,
            0,
        );
        stake_tx.sign(&validator);
        chain.add_transaction(stake_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        assert_eq!(chain.get_staked_balance(&address), 10_000_000);
        let balance_after_stake = chain.get_balance(&address);

        // Unstake 5M
        let mut unstake_tx = Transaction::unstake(
            chain.chain_id(),
            validator.public_key.clone(),
            5_000_000,
            5,
            1,
        );
        unstake_tx.sign(&validator);
        chain.add_transaction(unstake_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Staked reduced by 5M and funds remain locked until the unstake delay expires.
        assert_eq!(chain.get_staked_balance(&address), 5_000_000);
        assert_eq!(chain.get_account(&address).pending_unstakes.len(), 1);
        let balance_after_unstake_block = chain.get_balance(&address);
        // Balance includes block reward but minus gas fees for unstake tx
        assert!(
            balance_after_unstake_block < balance_after_stake + DEFAULT_BLOCK_REWARD - 5_000_000
        );
        assert!(
            balance_after_unstake_block
                > balance_after_stake + DEFAULT_BLOCK_REWARD - 5_000_000 - 100_000
        );

        for _ in 0..DEFAULT_UNSTAKE_DELAY_BLOCKS {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        assert!(chain.get_balance(&address) >= balance_after_unstake_block + 5_000_000);
    }

    #[test]
    fn test_rejects_invalid_state_root() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        let mut block = chain.create_block(&validator).unwrap();
        block.header.state_root = vec![7; 32];
        block.hash = Block::compute_hash(&block.header);
        block.signature = Some(validator.sign(&Block::signable_block_hash(&block.hash)));

        let err = chain.add_block(block).unwrap_err();
        assert!(matches!(err, ChainError::InvalidStateRoot));
    }

    #[test]
    fn test_rejects_unknown_finality_vote_hash() {
        let mut chain = Blockchain::new();
        let voter = KeyPair::generate();
        let vote = FinalityVote::new(hash::sha3_hash(b"unknown"), 1, 0, &voter);
        assert!(chain.add_finality_vote(vote).is_none());
    }

    #[test]
    fn test_rejects_invalid_fork_state_root() {
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-fork-test".to_string(),
            chain_name: "curs3d-fork-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let mut fork = chain.create_block(&validator).unwrap();
        fork.header.height = 1;
        fork.header.prev_hash = chain.genesis_block().hash;
        fork.header.state_root = vec![7; 32];
        fork.hash = Block::compute_hash(&fork.header);
        fork.signature = Some(validator.sign(&Block::signable_block_hash(&fork.hash)));

        let err = chain.add_block_with_fork_choice(fork).unwrap_err();
        assert!(matches!(err, ChainError::InvalidStateRoot));
    }

    #[test]
    fn test_deploy_contract() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        // Mine a block to get funds
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 7)
        )"#
        .to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            1_000_000,
            0,
            0,
        )
        .with_fee_caps(20, 2);
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Verify contract was stored and receipt exists
        assert_eq!(chain.contracts.len(), 1);
        assert!(!chain.receipts.is_empty());

        let receipt = chain.receipts.values().next().unwrap();
        assert!(receipt.success);
        assert!(receipt.contract_address.is_some());
        assert_eq!(receipt.contract_address.as_ref().unwrap().len(), 20);
        assert!(receipt.gas_used > 0);
    }

    #[test]
    fn test_call_contract() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        // Mine a block to get funds
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 7)
        )"#
        .to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            1_000_000,
            0,
            0,
        )
        .with_fee_caps(20, 2);
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Get the contract address from the receipt
        let contract_address = chain
            .receipts
            .values()
            .find(|r| r.contract_address.is_some())
            .unwrap()
            .contract_address
            .clone()
            .unwrap();

        // Call the contract
        let mut call_tx = Transaction::call_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            contract_address,
            b"do_something".to_vec(),
            0,
            1_000_000,
            0,
            1,
        )
        .with_fee_caps(20, 3);
        call_tx.sign(&validator);
        chain.add_transaction(call_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Should have 2 receipts now (deploy + call)
        assert_eq!(chain.receipts.len(), 2);
        let call_receipt = chain
            .receipts
            .values()
            .find(|r| r.contract_address.is_none())
            .unwrap();
        assert!(call_receipt.success);
        assert!(call_receipt.gas_used > 0);
        assert_eq!(call_receipt.return_data, 7i32.to_le_bytes().to_vec());
        assert!(call_receipt.effective_gas_price >= chain.current_base_fee_per_gas());
        assert!(call_receipt.gas_refunded > 0);
        assert_eq!(
            call_receipt.priority_fee_paid
                + call_receipt.base_fee_burned
                + call_receipt.gas_refunded,
            20 * 1_000_000
        );
    }

    #[test]
    fn test_async_persistence_does_not_write_on_each_block() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-async-persist-test".to_string(),
            chain_name: "curs3d-async-persist-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            epoch_length: 8,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let data_dir = dir.path().to_str().unwrap();
        let mut chain =
            Blockchain::with_storage_async_persistence(data_dir, Some(&genesis)).unwrap();

        let stored_before = chain.storage.as_ref().unwrap().get_height().unwrap();
        assert_eq!(stored_before, Some(0));

        for _ in 0..3 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        let stored_after = chain.storage.as_ref().unwrap().get_height().unwrap();
        assert_eq!(
            stored_after,
            Some(0),
            "live async mode must not synchronously write sled on each block"
        );
        assert_eq!(chain.height(), 3);
    }

    #[test]
    fn test_async_persistence_drop_timeout_does_not_block_indefinitely() {
        let (sender, receiver) = sync_channel::<PersistJob>(1);
        let handle = thread::spawn(move || {
            let _ = receiver.recv();
            thread::sleep(Duration::from_secs(60));
        });
        let persistence = PersistenceHandle {
            full_state_slot: Arc::new(StdMutex::new(None)),
            signal_sender: sender,
            join_handle: StdMutex::new(Some(handle)),
        };

        let started = std::time::Instant::now();
        drop(persistence);
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "PersistenceHandle::drop must timeout instead of blocking process shutdown"
        );
    }

    /// Regression for the post-incident audit: write enough blocks to cross
    /// at least one epoch boundary, drop the async chain (which triggers
    /// `Drop::drop` on `PersistenceHandle` → Shutdown signal → drain →
    /// join), reopen, and verify the persisted state matches what was in
    /// memory. Catches:
    ///   - latest-wins FullState slot losing data (it should NEVER lose);
    ///   - graceful shutdown not actually flushing the slot;
    ///   - reopen taking a stale snapshot.
    #[test]
    fn test_async_persistence_reopen_intact_after_epoch_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-async-reopen-test".to_string(),
            chain_name: "curs3d-async-reopen-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            // Small epoch so we cross multiple boundaries inside a unit test.
            epoch_length: 4,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let data_dir = dir.path().to_str().unwrap();

        let last_hash = {
            let mut chain =
                Blockchain::with_storage_async_persistence(data_dir, Some(&genesis)).unwrap();
            // 12 blocks = 3 epochs (boundaries at 4, 8, 12) → at least three
            // FullState signals. The last MUST land on disk.
            for _ in 0..12 {
                let block = chain.create_block(&validator).unwrap();
                chain.add_block(block).unwrap();
            }
            assert_eq!(chain.height(), 12);
            chain.latest_hash().to_vec()
            // chain (and PersistenceHandle) drops here → Shutdown sentinel
            // is sent and the worker joins. The latest FullState in the
            // single-buffered slot is drained on the way out.
        };

        let reopened = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();
        assert_eq!(
            reopened.height(),
            12,
            "graceful shutdown must drain the FullState slot before exit"
        );
        assert_eq!(
            reopened.latest_hash(),
            last_hash.as_slice(),
            "reopened tip hash must match what was in memory at drop time"
        );
    }

    /// Regression for "queue full silently drops state". Hammer
    /// `persist_full_state` faster than the worker can drain by repeatedly
    /// calling it without yielding. The latest-wins slot guarantees the
    /// final state lands; older snapshots may be coalesced but are never
    /// processed staler than the tip.
    #[test]
    fn test_async_persistence_full_state_is_latest_wins_under_pressure() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-async-pressure-test".to_string(),
            chain_name: "curs3d-async-pressure-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            epoch_length: 1, // every block is an epoch boundary
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let data_dir = dir.path().to_str().unwrap();

        let final_hash = {
            let mut chain =
                Blockchain::with_storage_async_persistence(data_dir, Some(&genesis)).unwrap();
            for _ in 0..50 {
                let block = chain.create_block(&validator).unwrap();
                chain.add_block(block).unwrap();
            }
            assert_eq!(chain.height(), 50);
            chain.latest_hash().to_vec()
        };

        let reopened = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();
        assert_eq!(reopened.height(), 50, "final block must reach disk");
        assert_eq!(reopened.latest_hash(), final_hash.as_slice());
    }

    /// End-to-end: deploy a real SDK-compiled wasm contract through the chain
    /// (not via Vm directly), call it twice, and verify the receipt + state +
    /// indexes (block_hash_to_height, tx_hash_index, log_index) all stay
    /// consistent. Skipped if the .wasm hasn't been built yet so CI without the
    /// wasm32 target keeps passing.
    #[test]
    fn test_sdk_counter_via_chain_integration() {
        const COUNTER_WASM: &[u8] = include_bytes!(
            "../../../sdk/rust/examples/counter/target/wasm32-unknown-unknown/release/counter_contract.wasm"
        );
        if COUNTER_WASM.len() < 16 {
            return;
        }

        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        // Mine several blocks to give the validator enough liquid balance to pay
        // for the deploy + call gas budgets.
        for _ in 0..6 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        // Deploy the SDK contract via a real DeployContract tx
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            COUNTER_WASM.to_vec(),
            5_000_000,
            0,
            0,
        )
        .with_fee_caps(50, 5);
        deploy_tx.sign(&validator);
        let deploy_hash = deploy_tx.hash();
        chain.add_transaction(deploy_tx).unwrap();
        let block = chain.create_block(&validator).unwrap();
        let deploy_block_hash = block.hash.clone();
        let deploy_block_height = block.header.height;
        chain.add_block(block).unwrap();

        // Index integrity: deploy block + tx are both reachable in O(1)
        assert_eq!(
            chain.block_hash_to_height.get(&deploy_block_hash).copied(),
            Some(deploy_block_height)
        );
        assert_eq!(
            chain.tx_hash_index.get(&deploy_hash).map(|(h, _)| *h),
            Some(deploy_block_height)
        );

        let contract_address = chain
            .receipts
            .values()
            .find(|r| r.contract_address.is_some())
            .expect("deploy receipt missing")
            .contract_address
            .clone()
            .unwrap();

        // First call: counter goes 0 → 1
        let mut call1 = Transaction::call_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            contract_address.clone(),
            Vec::new(),
            0,
            2_000_000,
            0,
            1,
        )
        .with_fee_caps(50, 5);
        call1.sign(&validator);
        let call1_hash = call1.hash();
        chain.add_transaction(call1).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let receipt1 = chain
            .get_receipt(&call1_hash)
            .expect("call1 receipt missing");
        assert!(receipt1.receipt.success);
        assert_eq!(receipt1.receipt.logs.len(), 1);
        assert_eq!(receipt1.receipt.logs[0].topics[0], b"tick");
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&receipt1.receipt.logs[0].data[..8]);
        assert_eq!(u64::from_le_bytes(buf), 1);

        // Second call: counter goes 1 → 2 (proves storage persisted across blocks)
        let mut call2 = Transaction::call_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            contract_address.clone(),
            Vec::new(),
            0,
            2_000_000,
            0,
            2,
        )
        .with_fee_caps(50, 5);
        call2.sign(&validator);
        let call2_hash = call2.hash();
        chain.add_transaction(call2).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let receipt2 = chain
            .get_receipt(&call2_hash)
            .expect("call2 receipt missing");
        let mut buf2 = [0u8; 8];
        buf2.copy_from_slice(&receipt2.receipt.logs[0].data[..8]);
        assert_eq!(u64::from_le_bytes(buf2), 2);

        // Log index gets two `tick` entries on the same contract
        let logs = chain.query_logs(&LogFilter {
            contract: Some(contract_address),
            topic: Some(b"tick".to_vec()),
            topics: None,
            from_block: None,
            to_block: None,
            limit: Some(10),
        });
        assert_eq!(logs.len(), 2);
    }

    #[test]
    fn test_deploy_contract_rejects_oversize_wasm() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();
        // Mine a block to fund the validator
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // 257 KB of bytes prefixed with the wasm magic so the size check fires
        // before any wasm validation logic.
        let mut oversize = vec![0u8; MAX_CONTRACT_CODE_BYTES + 1024];
        oversize[..4].copy_from_slice(b"\0asm");
        let mut tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            oversize,
            5_000_000,
            0,
            0,
        )
        .with_fee_caps(50, 5);
        tx.sign(&validator);

        let err = chain.add_transaction(tx).unwrap_err();
        match err {
            ChainError::InvalidTransactionFormat(msg) => {
                assert!(
                    msg.contains("256 KB"),
                    "expected size-limit error, got: {}",
                    msg
                );
            }
            other => panic!("expected InvalidTransactionFormat, got {:?}", other),
        }
    }

    #[test]
    fn test_block_hash_index_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();

        let allocations = vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 0,
        }];
        let genesis_config = GenesisConfig {
            allocations,
            ..GenesisConfig::default()
        };

        let mut block_hashes = Vec::new();
        {
            let mut chain =
                Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis_config))
                    .unwrap();
            for _ in 0..3 {
                let block = chain.create_block(&validator).unwrap();
                block_hashes.push(block.hash.clone());
                chain.add_block(block).unwrap();
            }
            // Index populated in-memory after add_block
            for (i, h) in block_hashes.iter().enumerate() {
                assert_eq!(
                    chain.block_hash_to_height.get(h).copied(),
                    Some((i + 1) as u64)
                );
            }
        }
        // Reopen from disk: rebuild_canonical_state must repopulate the index
        let chain =
            Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis_config)).unwrap();
        for (i, h) in block_hashes.iter().enumerate() {
            assert_eq!(
                chain.block_hash_to_height.get(h).copied(),
                Some((i + 1) as u64),
                "block hash index must survive restart"
            );
        }
    }

    #[test]
    fn test_account_and_storage_proofs_roundtrip() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let wasm_code = br#"(module
            (import "curs3d" "storage_write_bytes" (func $storage_write_bytes (param i32 i32 i32 i32) (result i32)))
            (import "curs3d" "storage_read" (func $storage_read (param i32 i32 i32 i32) (result i32)))
            (import "curs3d" "emit_log_bytes" (func $emit_log_bytes (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "key1")
            (data (i32.const 16) "val1")
            (data (i32.const 32) "topic")
            (func (export "curs3d_call") (result i32)
                i32.const 0
                i32.const 4
                i32.const 16
                i32.const 4
                call $storage_write_bytes
                drop
                i32.const 0
                i32.const 4
                i32.const 64
                i32.const 16
                call $storage_read
                drop
                i32.const 32
                i32.const 5
                i32.const 64
                i32.const 4
                call $emit_log_bytes
                drop
                i32.const 1)
        )"#
        .to_vec();

        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            1_000_000,
            0,
            0,
        )
        .with_fee_caps(20, 2);
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let contract_address = chain
            .receipts
            .values()
            .find_map(|receipt| receipt.contract_address.clone())
            .unwrap();

        let mut call_tx = Transaction::call_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            contract_address.clone(),
            Vec::new(),
            0,
            1_000_000,
            0,
            1,
        )
        .with_fee_caps(20, 2);
        let call_tx_hash = call_tx.hash();
        call_tx.sign(&validator);
        chain.add_transaction(call_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let validator_address = hash::address_bytes_from_public_key(&validator.public_key);
        let account_proof = chain.get_account_proof(&validator_address).unwrap();
        assert!(Blockchain::verify_account_proof(&account_proof));

        let storage_proof = chain.get_storage_proof(&contract_address, b"key1").unwrap();
        assert_eq!(storage_proof.value, b"val1".to_vec());
        assert!(Blockchain::verify_storage_proof(&storage_proof));

        let indexed_receipt = chain.get_receipt(&call_tx_hash).unwrap();
        assert_eq!(indexed_receipt.block_height, 3);
        assert_eq!(indexed_receipt.receipt.logs.len(), 1);

        let logs = chain.query_logs(&LogFilter {
            contract: Some(contract_address.clone()),
            topic: Some(b"topic".to_vec()),
            topics: None,
            from_block: Some(0),
            to_block: Some(chain.height()),
            limit: Some(10),
        });
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].tx_hash, call_tx_hash);
        assert_eq!(logs[0].data, b"val1".to_vec());

        // Positional topics filter (eth-style): topic at index 0 must equal "topic"
        let logs_positional = chain.query_logs(&LogFilter {
            contract: Some(contract_address.clone()),
            topic: None,
            topics: Some(vec![Some(b"topic".to_vec())]),
            from_block: None,
            to_block: None,
            limit: Some(10),
        });
        assert_eq!(logs_positional.len(), 1);

        // Wildcard at position 0 matches everything
        let logs_wildcard = chain.query_logs(&LogFilter {
            contract: Some(contract_address.clone()),
            topic: None,
            topics: Some(vec![None]),
            from_block: None,
            to_block: None,
            limit: Some(10),
        });
        assert_eq!(logs_wildcard.len(), 1);

        // Wrong topic at position 0 matches nothing
        let logs_no_match = chain.query_logs(&LogFilter {
            contract: Some(contract_address),
            topic: None,
            topics: Some(vec![Some(b"other".to_vec())]),
            from_block: None,
            to_block: None,
            limit: Some(10),
        });
        assert_eq!(logs_no_match.len(), 0);
    }

    #[test]
    fn test_transactions_for_address_returns_sent_and_received() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        // Block 1: validator gets the coinbase reward
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Validator sends a transfer to a fresh address
        let recipient = vec![9u8; hash::ADDRESS_LEN];
        let mut transfer_tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            recipient.clone(),
            1_000,
            100,
            0,
        );
        transfer_tx.sign(&validator);
        let transfer_hash = transfer_tx.hash();
        chain.add_transaction(transfer_tx).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let validator_addr = hash::address_bytes_from_public_key(&validator.public_key);
        let validator_txs = chain.transactions_for_address(&validator_addr, None, None, 50);
        // Validator was sender of the transfer + recipient of coinbase blocks
        assert!(
            validator_txs
                .iter()
                .any(|(_, _, tx)| tx.hash() == transfer_hash),
            "should include the transfer the validator sent"
        );

        let recipient_txs = chain.transactions_for_address(&recipient, None, None, 50);
        assert_eq!(recipient_txs.len(), 1);
        assert_eq!(recipient_txs[0].2.hash(), transfer_hash);

        // Limit clamps the result
        let limited = chain.transactions_for_address(&validator_addr, None, None, 1);
        assert_eq!(limited.len(), 1);
    }

    #[test]
    fn test_gas_limit_exceeded() {
        let mut chain = Blockchain::new();
        let validator = KeyPair::generate();

        // Mine a block to get funds
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Try to deploy with gas_limit too low
        let wasm_code = b"\0asm\x01\x00\x00\x00".to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            100, // way too low
            100,
            0,
        );
        deploy_tx.sign(&validator);

        let err = chain.add_transaction(deploy_tx).unwrap_err();
        assert!(matches!(err, ChainError::VmError(_)));
    }

    #[test]
    fn test_epoch_snapshot_created_at_boundary() {
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-epoch-test".to_string(),
            chain_name: "curs3d-epoch-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: 4, // short epoch for testing
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        // Mine 4 blocks to hit epoch boundary
        for _ in 0..4 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        // After height 4, epoch 1 should have a snapshot
        assert!(chain.epoch_snapshots.contains_key(&1));
        let snapshot = chain.epoch_snapshots.get(&1).unwrap();
        assert_eq!(snapshot.epoch, 1);
        assert_eq!(snapshot.start_height, 4);
        assert!(!snapshot.validators.is_empty());
        assert!(snapshot.total_stake > 0);
    }

    #[test]
    fn test_validator_selection_uses_frozen_set() {
        use crate::consensus::ProofOfStake;

        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-frozen-test".to_string(),
            chain_name: "curs3d-frozen-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: 4,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        // Mine 4 blocks to create epoch snapshot
        for _ in 0..4 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        let snapshot = chain.epoch_snapshots.get(&1).unwrap();
        // The frozen set should contain the validator
        assert_eq!(snapshot.validators.len(), 1);
        assert_eq!(snapshot.validators[0].public_key, validator.public_key);

        // select_validator_from_snapshot should return the validator
        let selected =
            ProofOfStake::select_validator_from_snapshot(snapshot, 5, &chain.latest_hash());
        assert!(selected.is_some());
        assert_eq!(selected.unwrap().public_key, validator.public_key);
    }

    #[test]
    fn test_snapshot_create_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-snapshot-test".to_string(),
            chain_name: "curs3d-snapshot-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain =
            Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis)).unwrap();

        // Mine a few blocks
        for _ in 0..3 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }

        let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 9)
        )"#
        .to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            1_000_000,
            100,
            0,
        );
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        // Create a snapshot
        let manifest = chain.create_snapshot().unwrap();
        let chunks = chain.get_snapshot_chunks(manifest.height).unwrap();
        assert_eq!(manifest.height, 4);
        assert!(manifest.chunk_count > 0);
        assert_eq!(manifest.chunk_hashes.len(), manifest.chunk_count);

        // Verify state root matches
        let expected_root = Blockchain::compute_state_root_full(&chain.accounts, &chain.contracts);
        assert_eq!(manifest.state_root, expected_root);

        let mut restored = Blockchain::from_genesis(genesis).unwrap();
        restored.apply_snapshot(&manifest, &chunks).unwrap();
        assert_eq!(restored.accounts, chain.accounts);
        assert_eq!(restored.contracts, chain.contracts);
        assert_eq!(restored.receipts.len(), chain.receipts.len());
        for (tx_hash, receipt) in &chain.receipts {
            let restored_receipt = restored.receipts.get(tx_hash).unwrap();
            assert_eq!(restored_receipt.success, receipt.success);
            assert_eq!(restored_receipt.gas_used, receipt.gas_used);
            assert_eq!(restored_receipt.contract_address, receipt.contract_address);
            assert_eq!(restored_receipt.return_data, receipt.return_data);
        }
        assert_eq!(restored.height(), chain.height());
    }

    #[test]
    fn test_snapshot_uses_finalized_base_and_tracks_tip() {
        let validator = KeyPair::generate();
        let mut chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "snapshot-finalized-test".to_string(),
            chain_name: "snapshot-finalized-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        })
        .unwrap();

        let block1 = chain.create_block(&validator).unwrap();
        chain.add_block(block1.clone()).unwrap();
        chain
            .block_tree
            .set_finalized(block1.hash.clone(), block1.header.height);
        chain.finality_tracker =
            FinalityTracker::with_finalized(block1.header.height, block1.hash.clone());

        let block2 = chain.create_block(&validator).unwrap();
        chain.add_block(block2.clone()).unwrap();

        let manifest = chain.create_snapshot().unwrap();
        assert_eq!(manifest.height, 1);
        assert_eq!(manifest.latest_hash, block1.hash);
        assert_eq!(manifest.tip_height, 2);
        assert_eq!(manifest.tip_hash, block2.hash);
    }

    #[test]
    fn test_snapshot_rejects_tampered_chunk_proof() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-snapshot-proof-test".to_string(),
            chain_name: "curs3d-snapshot-proof-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain =
            Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis)).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let manifest = chain.create_snapshot().unwrap();
        let mut chunks = chain.get_snapshot_chunks(manifest.height).unwrap();
        if let Some(first) = chunks.first_mut() {
            if let Some(proof_hash) = first.proof.first_mut() {
                proof_hash[0] ^= 0xFF;
            } else {
                first.proof.push(vec![0xAA; 32]);
            }
        }

        let mut restored = Blockchain::from_genesis(genesis).unwrap();
        let err = restored.apply_snapshot(&manifest, &chunks).unwrap_err();
        assert!(matches!(err, ChainError::SnapshotError(_)));
    }

    /// Regression: divergent non-finalized local forks are recoverable by
    /// snapshot sync. This is how a restarted/late node escapes a local fork
    /// without an operator wipe, while finalized checkpoints remain protected
    /// by the next test.
    #[test]
    fn test_snapshot_replaces_divergent_non_finalized_suffix() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-snapshot-divergent-test".to_string(),
            chain_name: "curs3d-snapshot-divergent-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };

        // chain_a = our local node, only one block past genesis.
        let mut chain_a =
            Blockchain::with_storage(dir_a.path().to_str().unwrap(), Some(&genesis)).unwrap();
        let block = chain_a.create_block(&validator).unwrap();
        chain_a.add_block(block).unwrap();
        let local_block_1_hash = chain_a.block_at_height(1).unwrap().hash;

        // chain_b = remote peer with a divergent history, several blocks ahead.
        // To force divergence at height 1 (block creation is otherwise deterministic
        // when the validator and parent are identical), wait one second so the
        // block timestamp differs.
        std::thread::sleep(std::time::Duration::from_secs(1));
        let mut chain_b =
            Blockchain::with_storage(dir_b.path().to_str().unwrap(), Some(&genesis)).unwrap();
        for _ in 0..5 {
            let block = chain_b.create_block(&validator).unwrap();
            chain_b.add_block(block).unwrap();
        }
        // Sanity: chain_b's block at height 1 must disagree with chain_a's.
        assert_ne!(chain_b.block_at_height(1).unwrap().hash, local_block_1_hash);
        // chain_a is shorter than chain_b's snapshot tip, so the legacy
        // tip-only check cannot fire here.
        assert!(chain_a.height() < chain_b.height());

        let manifest_b = chain_b.create_snapshot().unwrap();
        let chunks_b = chain_b.get_snapshot_chunks(manifest_b.height).unwrap();
        chain_a.apply_snapshot(&manifest_b, &chunks_b).unwrap();
        assert_eq!(chain_a.height(), chain_b.height());
        assert_ne!(chain_a.block_at_height(1).unwrap().hash, local_block_1_hash);
        assert_eq!(
            chain_a.block_at_height(1).unwrap().hash,
            chain_b.block_at_height(1).unwrap().hash
        );
    }

    #[test]
    fn test_snapshot_rejects_divergent_finalized_checkpoint() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-snapshot-divergent-finalized".to_string(),
            chain_name: "curs3d-snapshot-divergent-finalized".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };

        let mut chain_a =
            Blockchain::with_storage(dir_a.path().to_str().unwrap(), Some(&genesis)).unwrap();
        let block = chain_a.create_block(&validator).unwrap();
        chain_a.add_block(block).unwrap();
        let local_block_1_hash = chain_a.block_at_height(1).unwrap().hash;
        chain_a
            .block_tree
            .set_finalized(local_block_1_hash.clone(), 1);
        chain_a.finality_tracker = FinalityTracker::with_finalized(1, local_block_1_hash.clone());

        std::thread::sleep(std::time::Duration::from_secs(1));
        let mut chain_b =
            Blockchain::with_storage(dir_b.path().to_str().unwrap(), Some(&genesis)).unwrap();
        for _ in 0..5 {
            let block = chain_b.create_block(&validator).unwrap();
            chain_b.add_block(block).unwrap();
        }
        assert_ne!(chain_b.block_at_height(1).unwrap().hash, local_block_1_hash);

        let manifest_b = chain_b.create_snapshot().unwrap();
        let chunks_b = chain_b.get_snapshot_chunks(manifest_b.height).unwrap();
        let err = chain_a.apply_snapshot(&manifest_b, &chunks_b).unwrap_err();
        assert!(matches!(err, ChainError::SnapshotError(_)));
        assert_eq!(chain_a.block_at_height(1).unwrap().hash, local_block_1_hash);
    }

    #[test]
    fn test_restart_restores_contracts_and_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-restart-test".to_string(),
            chain_name: "curs3d-restart-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };

        let data_dir = dir.path().join("chain_db");
        let data_dir_str = data_dir.to_str().unwrap();

        let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 11)
        )"#
        .to_vec();
        let mut deploy_tx = Transaction::deploy_contract(
            chain.chain_id(),
            validator.public_key.clone(),
            wasm_code,
            1_000_000,
            100,
            0,
        );
        deploy_tx.sign(&validator);
        chain.add_transaction(deploy_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let expected_contracts = chain.contracts.clone();
        let expected_receipts = chain.receipts.clone();
        let expected_height = chain.height();

        drop(chain);

        let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted.height(), expected_height);
        assert_eq!(restarted.contracts, expected_contracts);
        assert_eq!(restarted.receipts.len(), expected_receipts.len());
        for (tx_hash, receipt) in expected_receipts {
            let restored = restarted.receipts.get(&tx_hash).unwrap();
            assert_eq!(restored.success, receipt.success);
            assert_eq!(restored.gas_used, receipt.gas_used);
            assert_eq!(restored.contract_address, receipt.contract_address);
        }
    }

    /// Regression test for #2: build a chain, drop it, reload from disk, and
    /// verify the recomputed state root matches every persisted block header.
    /// If a non-determinism creeps into state-root computation (HashMap
    /// iteration order, leaky governance state, etc.) this will fail with
    /// `InvalidStateRoot` on the second `with_storage`.
    #[test]
    fn test_state_root_deterministic_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let recipient = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-state-root-restart".to_string(),
            chain_name: "curs3d-state-root-restart".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&validator.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
                GenesisAllocation {
                    public_key: hex::encode(&recipient.public_key),
                    balance: 0,
                    staked_balance: 0,
                },
            ],
            ..Default::default()
        };

        let data_dir = dir.path().join("chain_db");
        let data_dir_str = data_dir.to_str().unwrap();

        let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        // Build a few blocks with mixed activity (transfer + stake unwinding +
        // contract deploy) so the state graph has enough surface area to
        // expose non-determinism if any creeps in.
        let recipient_addr = hash::address_bytes_from_public_key(&recipient.public_key);
        for i in 0..5u64 {
            let mut tx = Transaction::new(
                chain.chain_id(),
                validator.public_key.clone(),
                recipient_addr.clone(),
                100,
                10,
                i,
            );
            tx.sign(&validator);
            chain.add_transaction(tx).unwrap();
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }
        let expected_height = chain.height();
        let expected_state_roots: Vec<Vec<u8>> =
            chain.iter_blocks().map(|b| b.header.state_root).collect();

        drop(chain);

        // Reloading must succeed; rebuild_canonical_state would otherwise
        // raise InvalidStateRoot during replay.
        let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted.height(), expected_height);
        for (h, expected_root) in expected_state_roots.iter().enumerate() {
            assert_eq!(
                &restarted
                    .block_at_height(h as u64)
                    .unwrap()
                    .header
                    .state_root,
                expected_root,
                "state_root for block {} diverged after restart",
                h
            );
        }
    }

    #[test]
    fn test_restart_restores_token_registry_and_governance() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-governance-token-test".to_string(),
            chain_name: "curs3d-governance-token-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };

        let data_dir = dir.path().join("chain_db");
        let data_dir_str = data_dir.to_str().unwrap();
        let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();

        let mut deploy_token_tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            Vec::new(),
            0,
            100,
            0,
        );
        deploy_token_tx.kind = TransactionKind::DeployToken;
        deploy_token_tx.data = serde_json::to_vec(&crate::token::DeployTokenParams {
            name: "Persisted Token".to_string(),
            symbol: "PST".to_string(),
            decimals: 6,
            total_supply: 1_000_000,
        })
        .unwrap();
        deploy_token_tx.sign(&validator);
        chain.add_transaction(deploy_token_tx).unwrap();

        let mut proposal_tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            Vec::new(),
            0,
            100,
            1,
        );
        proposal_tx.kind = TransactionKind::SubmitProposal;
        proposal_tx.data = serde_json::to_vec(&crate::governance::SubmitProposalParams {
            kind: crate::governance::ProposalKind::ParameterChange {
                parameter: "block_gas_limit".to_string(),
                new_value: 20_000_000,
            },
        })
        .unwrap();
        proposal_tx.sign(&validator);
        chain.add_transaction(proposal_tx).unwrap();

        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();

        let expected_registry = chain.token_registry.clone();
        let expected_governance_ids: Vec<Vec<u8>> = chain
            .governance
            .list_proposals()
            .iter()
            .map(|p| p.id.clone())
            .collect();

        assert_eq!(expected_registry.tokens.len(), 1);
        assert_eq!(expected_governance_ids.len(), 1);

        drop(chain);

        let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted.token_registry, expected_registry);
        let restarted_ids: Vec<Vec<u8>> = restarted
            .governance
            .list_proposals()
            .iter()
            .map(|p| p.id.clone())
            .collect();
        assert_eq!(restarted_ids, expected_governance_ids);
    }

    #[test]
    fn test_protocol_version_at_height() {
        let genesis = GenesisConfig {
            chain_id: "curs3d-version-test".to_string(),
            chain_name: "curs3d-version-test".to_string(),
            upgrades: vec![
                super::ProtocolUpgrade {
                    height: 10,
                    version: 2,
                    description: "Version 2 upgrade".to_string(),
                },
                super::ProtocolUpgrade {
                    height: 20,
                    version: 3,
                    description: "Version 3 upgrade".to_string(),
                },
            ],
            ..Default::default()
        };
        let chain = Blockchain::from_genesis(genesis).unwrap();

        assert_eq!(chain.protocol_version_at_height(0), 1);
        assert_eq!(chain.protocol_version_at_height(5), 1);
        assert_eq!(chain.protocol_version_at_height(10), 2);
        assert_eq!(chain.protocol_version_at_height(15), 2);
        assert_eq!(chain.protocol_version_at_height(20), 3);
        assert_eq!(chain.protocol_version_at_height(100), 3);
    }

    #[test]
    fn test_rejects_wrong_protocol_version() {
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-ver-reject-test".to_string(),
            chain_name: "curs3d-ver-reject-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        // Create a valid block then tamper with its version
        let mut block = chain.create_block(&validator).unwrap();
        block.header.version = 99; // Wrong version
        block.hash = Block::compute_hash(&block.header);
        block.signature = Some(validator.sign(&Block::signable_block_hash(&block.hash)));

        let err = chain.add_block(block).unwrap_err();
        assert!(matches!(err, ChainError::InvalidProtocolVersion { .. }));
    }

    #[test]
    fn test_block_rejected_if_wrong_proposer() {
        // Two validators in genesis with equal stake. After the legitimate
        // leader produces block 1 (advancing the parent timestamp to ~now),
        // the non-leader at height 2 must be rejected with WrongProposer
        // because no backup-leader timeout has elapsed yet.
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-wrong-proposer-test".to_string(),
            chain_name: "curs3d-wrong-proposer-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&kp_a.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
                GenesisAllocation {
                    public_key: hex::encode(&kp_b.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
            ],
            ..Default::default()
        };
        let mut chain = Blockchain::from_genesis(genesis).unwrap();

        // Mine block 1 with whichever validator is elected at height 1. This
        // refreshes the parent timestamp to ~now so the rank-0 window is
        // active for height 2.
        let leader_h1 = chain
            .slot_leader_address(1, &chain.latest_hash(), 0)
            .expect("slot leader at height 1");
        let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
        let real_kp_h1 = if leader_h1 == addr_a { &kp_a } else { &kp_b };
        let block1 = chain.create_block(real_kp_h1).unwrap();
        chain.add_block(block1).unwrap();

        // Now identify the rank-0 leader at height 2 and pick the *other*
        // keypair as the imposter.
        let leader_h2 = chain
            .slot_leader_address(2, &chain.latest_hash(), 0)
            .expect("slot leader at height 2");
        let imposter_kp = if leader_h2 == addr_a { &kp_b } else { &kp_a };

        // The non-leader's create_block fails fast with WrongProposer.
        let err = chain
            .create_block(imposter_kp)
            .expect_err("non-leader should not be allowed to produce");
        assert!(
            matches!(err, ChainError::WrongProposer { height: 2, .. }),
            "expected WrongProposer at height 2, got {:?}",
            err
        );
    }

    #[test]
    fn test_height_one_is_primary_only_even_if_genesis_timestamp_is_old() {
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
        let genesis = GenesisConfig {
            chain_id: "curs3d-height-one-primary-only".to_string(),
            chain_name: "curs3d-height-one-primary-only".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&kp_a.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
                GenesisAllocation {
                    public_key: hex::encode(&kp_b.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
            ],
            ..Default::default()
        };
        let chain = Blockchain::from_genesis(genesis).unwrap();
        let leader_h1 = chain
            .slot_leader_address(1, &chain.latest_hash(), 0)
            .expect("height-1 leader");
        let backup_kp = if leader_h1 == addr_a { &kp_b } else { &kp_a };
        let err = chain
            .create_block(backup_kp)
            .expect_err("height-1 backup must not be authorized");
        assert!(matches!(err, ChainError::WrongProposer { height: 1, .. }));
    }

    #[test]
    fn test_two_validators_alternate() {
        // Sample a long run of slot-leader picks across two equal-stake
        // validators. Verify a single block per height (no fork) and a roughly
        // 50/50 split. With only `slot_leader` as the gate, both validators
        // converge to the same producer per height.
        use crate::consensus::slot_leader;
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
        let addr_b = hash::address_bytes_from_public_key(&kp_b.public_key);
        let genesis = GenesisConfig {
            chain_id: "curs3d-two-validator-test".to_string(),
            chain_name: "curs3d-two-validator-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&kp_a.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
                GenesisAllocation {
                    public_key: hex::encode(&kp_b.public_key),
                    balance: 1_000_000_000,
                    staked_balance: 5_000,
                },
            ],
            ..Default::default()
        };
        let chain = Blockchain::from_genesis(genesis).unwrap();
        // Walk a synthetic 30-slot ledger. Each height: only the elected
        // leader produces — exactly one block per height — and the other
        // validator stays quiet. 30 samples make a degenerate 30/0 split
        // astronomically unlikely (P < 1e-9) for an honest hash function.
        // We go through `slot_leader_address` so the per-height live-snapshot
        // fallback applies (the genesis-epoch cached snapshot is empty by
        // construction; see `snapshot_for_height`).
        let _ = slot_leader; // imported for symmetry with the bare-snapshot API
        let mut leaders: Vec<Vec<u8>> = Vec::new();
        let parent = chain.latest_hash().to_vec();
        for h in 1..=30 {
            let leader = chain.slot_leader_address(h, &parent, 0).unwrap();
            leaders.push(leader);
        }

        // Single producer per height (vacuously true here, but proves the
        // function returns a unique address per slot).
        for leader in &leaders {
            assert!(*leader == addr_a || *leader == addr_b);
        }

        let count_a = leaders.iter().filter(|a| **a == addr_a).count();
        let count_b = leaders.iter().filter(|a| **a == addr_b).count();
        assert_eq!(count_a + count_b, 30);
        // 50/50 in expectation; allow any non-degenerate split.
        assert!(count_a > 0, "validator A never produced");
        assert!(count_b > 0, "validator B never produced");
        // Ratio within ±50% of expected 15 (very loose to absorb sampling).
        assert!(
            (5..=25).contains(&count_a),
            "split too skewed: a={}, b={}",
            count_a,
            count_b
        );
    }

    /// Regression test for the `state_root_mismatch` bug at the second epoch
    /// boundary on multi-validator chains.
    ///
    /// Symptom: a node that has lived past `h = 2 * epoch_length` (the first
    /// height where epoch settlement actually distributes rewards — the
    /// `prev_epoch > 0` guard in `add_block` skips settlement at the very
    /// first epoch boundary `h = epoch_length`) crashes on restart with
    /// `Failed to initialize blockchain storage: invalid state root`.
    ///
    /// Root cause: `add_block` mutates `self.accounts` via
    /// `consensus::apply_epoch_settlement` *before* calling
    /// `validate_block_against_state` — the live state root therefore reflects
    /// post-settlement balances. But the boot path
    /// (`with_storage` -> `rebuild_canonical_state`) replays each block via
    /// `validate_block_against_state` *without* applying the same settlement
    /// step on the parent accounts handed in. The recomputed state root for
    /// the boundary block is therefore strictly less (rewards never granted)
    /// and the chain refuses to load.
    ///
    /// This test reproduces the failure with a 2-validator genesis (similar
    /// to the live testnet) and a small `epoch_length` of 4, mining past
    /// `h = 2 * epoch_length = 8` so that the *second* epoch boundary
    /// settles non-zero rewards. With the bug, the second `with_storage`
    /// returns `ChainError::InvalidStateRoot`. With the fix it succeeds.
    #[test]
    fn test_restart_across_epoch_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
        // Use a tiny epoch length so we cross the *second* boundary quickly.
        // EPOCH_REWARD_RATE_PER_CUR is 100 microtokens per CUR per block, so
        // we need staked >= 1 CUR (= 1_000_000 microtokens) for rewards to
        // be non-zero — the bug is otherwise masked by the saturating
        // arithmetic.
        let epoch_length: u64 = 4;
        let stake = 1_000_000_000u64; // 1000 CUR — comfortably above minimum_stake.
        let genesis = GenesisConfig {
            chain_id: "curs3d-restart-epoch-test".to_string(),
            chain_name: "curs3d-restart-epoch-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            allocations: vec![
                GenesisAllocation {
                    public_key: hex::encode(&kp_a.public_key),
                    balance: 1_000_000_000,
                    staked_balance: stake,
                },
                GenesisAllocation {
                    public_key: hex::encode(&kp_b.public_key),
                    balance: 1_000_000_000,
                    staked_balance: stake,
                },
            ],
            ..Default::default()
        };
        let data_dir = dir.path().join("chain_db");
        let data_dir_str = data_dir.to_str().unwrap();

        // Mine across the first *two* epoch boundaries so the bug actually
        // fires. Settlement at h=epoch_length is skipped (prev_epoch == 0),
        // settlement at h=2*epoch_length runs and grants rewards. After that
        // any restart re-validates block 2*epoch_length and trips the bug.
        let target_height = 2 * epoch_length + 1;
        let expected_state_roots: Vec<Vec<u8>>;
        let expected_height: u64;
        {
            let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
            for h in 1..=target_height {
                let leader = chain
                    .slot_leader_address(h, &chain.latest_hash(), 0)
                    .expect("slot leader exists");
                let kp = if leader == addr_a { &kp_a } else { &kp_b };
                let block = chain.create_block(kp).unwrap();
                chain.add_block(block).unwrap();
            }
            expected_height = chain.height();
            expected_state_roots = chain.iter_blocks().map(|b| b.header.state_root).collect();
            assert_eq!(expected_height, target_height);
        }

        // Reload — this is what fails with `invalid state root` in the wild.
        let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted.height(), expected_height);
        for (h, expected_root) in expected_state_roots.iter().enumerate() {
            assert_eq!(
                &restarted
                    .block_at_height(h as u64)
                    .unwrap()
                    .header
                    .state_root,
                expected_root,
                "state_root for block {} diverged after restart",
                h
            );
        }
        // Open a third time as paranoia: ensure replay is idempotent.
        drop(restarted);
        let restarted_again = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        assert_eq!(restarted_again.height(), expected_height);
    }

    // ───────── Mempool priority classes ─────────

    #[test]
    fn mempool_class_assignment_per_kind() {
        // System: validator-set or governance-affecting.
        assert_eq!(TransactionKind::Stake.mempool_class(), MempoolClass::System);
        assert_eq!(
            TransactionKind::Unstake.mempool_class(),
            MempoolClass::System
        );
        assert_eq!(
            TransactionKind::SubmitProposal.mempool_class(),
            MempoolClass::System
        );
        assert_eq!(
            TransactionKind::GovernanceVote.mempool_class(),
            MempoolClass::System
        );

        // User: everything else, including EVM.
        assert_eq!(
            TransactionKind::Transfer.mempool_class(),
            MempoolClass::User
        );
        assert_eq!(
            TransactionKind::DeployContract.mempool_class(),
            MempoolClass::User
        );
        assert_eq!(
            TransactionKind::CallContract.mempool_class(),
            MempoolClass::User
        );
        assert_eq!(
            TransactionKind::DeployToken.mempool_class(),
            MempoolClass::User
        );
        assert_eq!(
            TransactionKind::TokenTransfer.mempool_class(),
            MempoolClass::User
        );
        assert_eq!(
            TransactionKind::TokenApprove.mempool_class(),
            MempoolClass::User
        );
        assert_eq!(
            TransactionKind::TokenTransferFrom.mempool_class(),
            MempoolClass::User
        );
        assert_eq!(
            TransactionKind::DeployEvmContract.mempool_class(),
            MempoolClass::User
        );
        assert_eq!(
            TransactionKind::CallEvmContract.mempool_class(),
            MempoolClass::User
        );
    }

    /// Build a chain whose first allocation is a producer (so we can mine
    /// blocks) and the rest are funded users with enough liquid to
    /// transfer + enough staked-or-liquid to stake.
    fn priority_class_chain(extra_funders: usize) -> (Blockchain, KeyPair, Vec<KeyPair>) {
        let producer = KeyPair::generate();
        let funders: Vec<KeyPair> = (0..extra_funders).map(|_| KeyPair::generate()).collect();
        let mut allocations = vec![GenesisAllocation {
            public_key: hex::encode(&producer.public_key),
            balance: 1_000_000_000_000,
            staked_balance: 100_000_000_000,
        }];
        for kp in &funders {
            allocations.push(GenesisAllocation {
                public_key: hex::encode(&kp.public_key),
                balance: 1_000_000_000_000,
                staked_balance: 0,
            });
        }
        let chain = Blockchain::from_genesis(GenesisConfig {
            chain_id: "mempool-class-test".to_string(),
            chain_name: "mempool-class-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
            epoch_length: DEFAULT_EPOCH_LENGTH,
            jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
            block_gas_limit: 100_000,
            allocations,
            ..Default::default()
        })
        .unwrap();
        (chain, producer, funders)
    }

    #[test]
    fn count_pending_by_class_tracks_both_pools() {
        let (mut chain, producer, funders) = priority_class_chain(2);
        // Mine block 1 so producer can submit (nonce machinery).
        let block = chain.create_block(&producer).unwrap();
        chain.add_block(block).unwrap();

        // 2 user txs (Transfer) + 1 system tx (Stake).
        let recipient = hash::address_bytes_from_public_key(&KeyPair::generate().public_key);
        for (i, kp) in funders.iter().enumerate() {
            let mut tx = Transaction::new(
                chain.chain_id(),
                kp.public_key.clone(),
                recipient.clone(),
                10_000,
                10,
                0,
            );
            tx.sign(kp);
            chain.add_transaction(tx).unwrap();
            assert_eq!(chain.pending_transactions.len(), i + 1);
        }
        let mut stake_tx = Transaction::stake(
            chain.chain_id(),
            funders[0].public_key.clone(),
            2_000,
            10,
            1,
        );
        stake_tx.sign(&funders[0]);
        chain.add_transaction(stake_tx).unwrap();

        let (system, user) = chain.count_pending_by_class();
        assert_eq!(system, 1, "1 Stake = 1 system");
        assert_eq!(user, 2, "2 Transfer = 2 user");
    }

    #[test]
    fn sort_pending_puts_system_class_first() {
        let (mut chain, producer, funders) = priority_class_chain(3);
        let block = chain.create_block(&producer).unwrap();
        chain.add_block(block).unwrap();

        // Add in the order [Transfer, Stake, Transfer] — different
        // senders so the sort can freely reorder by class.
        let recipient = hash::address_bytes_from_public_key(&KeyPair::generate().public_key);

        let mut tx_user_a = Transaction::new(
            chain.chain_id(),
            funders[0].public_key.clone(),
            recipient.clone(),
            10_000,
            50,
            0,
        );
        tx_user_a.sign(&funders[0]);
        chain.add_transaction(tx_user_a).unwrap();

        // System tx with intentionally LOWER fee — class ordering must
        // override fee priority for inter-class comparisons.
        let mut tx_system = Transaction::stake(
            chain.chain_id(),
            funders[1].public_key.clone(),
            2_000,
            10,
            0,
        );
        tx_system.sign(&funders[1]);
        let system_tx_hash = tx_system.hash();
        chain.add_transaction(tx_system).unwrap();

        let mut tx_user_b = Transaction::new(
            chain.chain_id(),
            funders[2].public_key.clone(),
            recipient,
            10_000,
            50,
            0,
        );
        tx_user_b.sign(&funders[2]);
        chain.add_transaction(tx_user_b).unwrap();

        // After sort, the system tx must be at position 0 even though
        // its fee is lower than the user txs.
        assert_eq!(
            chain.pending_transactions[0].hash(),
            system_tx_hash,
            "system tx must sort to the front of the pending vec, regardless of fee"
        );
        assert_eq!(
            chain.pending_transactions[0].mempool_class(),
            MempoolClass::System
        );
        assert_eq!(
            chain.pending_transactions[1].mempool_class(),
            MempoolClass::User
        );
        assert_eq!(
            chain.pending_transactions[2].mempool_class(),
            MempoolClass::User
        );
    }

    #[test]
    fn worst_pending_in_class_only_returns_that_class() {
        // The class-restricted eviction helper is the lynchpin of the
        // "System class never evicted by user pressure" invariant. This
        // unit test pins its behaviour: it must only return indices
        // matching the requested class, regardless of relative fees.
        let (mut chain, producer, funders) = priority_class_chain(3);
        let block = chain.create_block(&producer).unwrap();
        chain.add_block(block).unwrap();

        let recipient = hash::address_bytes_from_public_key(&KeyPair::generate().public_key);

        // System tx with a LOW fee — would be the worst candidate
        // overall if class wasn't filtered.
        let mut stake_tx =
            Transaction::stake(chain.chain_id(), funders[0].public_key.clone(), 2_000, 5, 0);
        stake_tx.sign(&funders[0]);
        let stake_hash = stake_tx.hash();
        chain.add_transaction(stake_tx).unwrap();

        // User tx with a HIGHER fee than the system tx.
        let mut tx_user = Transaction::new(
            chain.chain_id(),
            funders[1].public_key.clone(),
            recipient,
            1_000,
            50,
            0,
        );
        tx_user.sign(&funders[1]);
        let user_hash = tx_user.hash();
        chain.add_transaction(tx_user).unwrap();

        // worst-in-User returns the user tx (only user-class entry).
        let idx_user = chain
            .worst_pending_transaction_index_in_class(MempoolClass::User)
            .expect("user-class worst must exist");
        assert_eq!(chain.pending_transactions[idx_user].hash(), user_hash);
        assert_eq!(
            chain.pending_transactions[idx_user].mempool_class(),
            MempoolClass::User
        );

        // worst-in-System returns the stake tx (only system-class
        // entry) — never the user tx, even though the user has a
        // higher fee that would normally lose the "worst" race.
        let idx_system = chain
            .worst_pending_transaction_index_in_class(MempoolClass::System)
            .expect("system-class worst must exist");
        assert_eq!(chain.pending_transactions[idx_system].hash(), stake_hash);
        assert_eq!(
            chain.pending_transactions[idx_system].mempool_class(),
            MempoolClass::System
        );
    }

    // ───────── v6 SparseMerkleTrie state root ─────────

    fn account_with_balance(balance: u64) -> AccountState {
        AccountState {
            balance,
            nonce: 0,
            staked_balance: 0,
            pending_unstakes: Vec::new(),
            validator_active_from_height: 0,
            jailed_until_height: 0,
            public_key: None,
        }
    }

    #[test]
    fn v6_smt_root_is_deterministic_under_insertion_order() {
        let addr_a = vec![0x01; 20];
        let addr_b = vec![0x02; 20];
        let addr_c = vec![0x03; 20];

        let mut accounts_1 = HashMap::new();
        accounts_1.insert(addr_a.clone(), account_with_balance(100));
        accounts_1.insert(addr_b.clone(), account_with_balance(200));
        accounts_1.insert(addr_c.clone(), account_with_balance(300));

        let mut accounts_2 = HashMap::new();
        accounts_2.insert(addr_c, account_with_balance(300));
        accounts_2.insert(addr_a, account_with_balance(100));
        accounts_2.insert(addr_b, account_with_balance(200));

        let root_1 = Blockchain::compute_state_root_at_protocol(&accounts_1, &HashMap::new(), 6);
        let root_2 = Blockchain::compute_state_root_at_protocol(&accounts_2, &HashMap::new(), 6);
        assert_eq!(
            root_1, root_2,
            "v6 SMT root must be independent of HashMap iteration order"
        );
    }

    #[test]
    fn v6_smt_root_differs_from_v5_merkle_root() {
        // Different commitment schemes → different bytes for non-empty
        // state. (The empty-state case is intentionally allowed to
        // differ — see the documentation on `compute_state_root_v6_smt`.)
        let mut accounts = HashMap::new();
        accounts.insert(vec![0x42; 20], account_with_balance(1_000_000));

        let v5 = Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 5);
        let v6 = Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 6);
        assert_ne!(v5, v6, "v5 and v6 commitments must produce different bytes");
        assert_eq!(v6.len(), 32, "v6 SMT root must be 32 bytes");
    }

    #[test]
    fn dispatch_below_v6_uses_legacy_merkle() {
        // For any protocol version < 6 the dispatcher must produce the
        // EXACT same bytes as the legacy compute_state_root_full.
        // Without this guarantee, switching call sites to the dispatcher
        // would silently change the bytes that go on-chain — a hardfork
        // in disguise.
        let mut accounts = HashMap::new();
        accounts.insert(vec![0x05; 20], account_with_balance(500));
        accounts.insert(vec![0x06; 20], account_with_balance(600));

        let legacy = Blockchain::compute_state_root_full(&accounts, &HashMap::new());
        let dispatched_v1 =
            Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 1);
        let dispatched_v3 =
            Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 3);
        let dispatched_v5 =
            Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 5);
        assert_eq!(dispatched_v1, legacy);
        assert_eq!(dispatched_v3, legacy);
        assert_eq!(dispatched_v5, legacy);
    }

    #[test]
    fn v6_hardfork_height_is_dormant_at_baseline() {
        // The hardfork constant ships as `u64::MAX`. Make sure
        // protocol_version_at_height never returns v6 unless the
        // constant is explicitly lowered or a genesis upgrade asks for
        // it. Regression guard against an accidental constant bump.
        assert_eq!(V6_HARDFORK_HEIGHT_TESTNET, u64::MAX);
        let chain = Blockchain::new();
        assert_eq!(chain.protocol_version_at_height(0), 5);
        assert_eq!(chain.protocol_version_at_height(1), 5);
        assert_eq!(chain.protocol_version_at_height(1_000_000), 5);
        assert_eq!(chain.protocol_version_at_height(u64::MAX - 1), 5);
        // Only the absolute top, which is intentionally unreachable in
        // a real chain, would trigger v6 with the current constant.
        assert_eq!(
            chain.protocol_version_at_height(u64::MAX),
            V6_PROTOCOL_VERSION
        );
    }

    #[test]
    fn worst_pending_in_empty_class_returns_none() {
        // If a class is empty, the helper returns None and the eviction
        // loop falls through to the other class (via the .or_else chain
        // in enforce_mempool_limits).
        let (mut chain, producer, funders) = priority_class_chain(1);
        let block = chain.create_block(&producer).unwrap();
        chain.add_block(block).unwrap();

        let recipient = hash::address_bytes_from_public_key(&KeyPair::generate().public_key);
        let mut tx_user = Transaction::new(
            chain.chain_id(),
            funders[0].public_key.clone(),
            recipient,
            1_000,
            10,
            0,
        );
        tx_user.sign(&funders[0]);
        chain.add_transaction(tx_user).unwrap();

        // No system txs in pool.
        assert!(
            chain
                .worst_pending_transaction_index_in_class(MempoolClass::System)
                .is_none()
        );
        // User-class lookup finds the lone transfer.
        assert!(
            chain
                .worst_pending_transaction_index_in_class(MempoolClass::User)
                .is_some()
        );
    }

    /// Post-D.3 the cursor IS the canonical block view. The Phase-B
    /// dual-write coherence assertion is gone (nothing to compare
    /// against). This replacement test exercises the same scenario but
    /// asserts cursor self-consistency: every block added via
    /// `add_block` must be readable from the cursor at its height, the
    /// `block_count` must track `height + 1`, and the cursor's view of
    /// the head must match the head a fresh `with_storage` reload sees.
    #[test]
    fn cursor_is_canonical_block_view_after_add_block() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-cursor-canonical-test".to_string(),
            chain_name: "curs3d-cursor-canonical-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            epoch_length: 8,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let data_dir = dir.path().to_str().unwrap();
        let mut chain = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();

        // Track the hashes we just produced so we can verify the cursor
        // (and a fresh reload) report the same chain.
        let mut expected_hashes = vec![chain.genesis_block().hash];
        for _ in 0..10 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block.clone()).unwrap();
            expected_hashes.push(block.hash);

            let head = chain.height();
            assert_eq!(chain.block_count(), head + 1);

            for h in 0..=head {
                let read = chain
                    .block_at_height(h)
                    .unwrap_or_else(|| panic!("cursor missing block at height {h}"));
                assert_eq!(
                    read.hash, expected_hashes[h as usize],
                    "cursor hash mismatch at height {h}"
                );
                assert_eq!(read.header.height, h);
            }
        }

        // After a fresh reload (rebuilds the cursor from redb), the chain
        // must see the same blocks. This catches any persistence bug that
        // wouldn't show up while the cursor's in-memory cache still has
        // the blocks.
        drop(chain);
        let reopened = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();
        assert_eq!(reopened.block_count(), expected_hashes.len() as u64);
        for (h, expected) in expected_hashes.iter().enumerate() {
            let read = reopened
                .block_at_height(h as u64)
                .unwrap_or_else(|| panic!("reload missing block at height {h}"));
            assert_eq!(&read.hash, expected, "reload hash mismatch at height {h}");
        }
    }

    /// #28 Phase E — `maybe_prune_finalized` drops history below
    /// `finalized - keep_blocks`. Archival mode (None) is a no-op. With
    /// `Some(K)`, finalizing block F prunes [1, F-K). Genesis (h=0)
    /// always stays pinned. The cursor's storage backend (redb in this
    /// test) must report the pruned heights as gone too — otherwise
    /// the disk usage never shrinks.
    #[test]
    fn maybe_prune_finalized_drops_history_below_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let validator = KeyPair::generate();
        let genesis = GenesisConfig {
            chain_id: "curs3d-prune-test".to_string(),
            chain_name: "curs3d-prune-test".to_string(),
            block_reward: DEFAULT_BLOCK_REWARD,
            minimum_stake: 1_000,
            epoch_length: 8,
            allocations: vec![GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            }],
            ..Default::default()
        };
        let data_dir = dir.path().to_str().unwrap();
        let mut chain = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();

        // Build a chain of 20 blocks so we have a meaningful prune target.
        for _ in 0..20 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }
        assert_eq!(chain.height(), 20);
        assert_eq!(chain.chain_base_height(), 0, "archival mode = no prune");

        // Archival mode: no prune even with a finalization event.
        let removed = chain.maybe_prune_finalized(15);
        assert_eq!(removed, 0);
        assert_eq!(chain.chain_base_height(), 0);
        assert!(chain.block_at_height(5).is_some(), "no prune in archival");

        // Enable pruning with a 5-block retention window. Finalize at
        // height 15 → keep heights [10, 15], drop heights [1, 9].
        chain.set_prune_keep_blocks(Some(5));
        assert_eq!(chain.prune_keep_blocks(), Some(5));
        let removed = chain.maybe_prune_finalized(15);
        assert!(removed > 0, "should have pruned some history");
        assert_eq!(chain.chain_base_height(), 10);

        // Pruned heights return None; retained heights still resolve.
        for h in 1..10 {
            assert!(
                chain.block_at_height(h).is_none(),
                "height {h} should be pruned"
            );
        }
        for h in 10..=20 {
            assert!(
                chain.block_at_height(h).is_some(),
                "height {h} must still resolve"
            );
        }
        // Genesis is always preserved (pinned by cursor).
        assert!(chain.block_at_height(0).is_some());

        // A second prune at the same finalized height is a no-op.
        let removed = chain.maybe_prune_finalized(15);
        assert_eq!(removed, 0);
        assert_eq!(chain.chain_base_height(), 10);

        // Disable pruning again: future finalizations don't extend the
        // prune watermark.
        chain.set_prune_keep_blocks(None);
        let removed = chain.maybe_prune_finalized(20);
        assert_eq!(removed, 0);
        assert_eq!(chain.chain_base_height(), 10, "watermark unchanged");
    }
}
