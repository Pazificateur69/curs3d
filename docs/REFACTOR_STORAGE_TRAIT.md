# Refactor — `Storage` → `dyn PersistenceBackend`

**Status**: Design.
**Tracking task**: #31.
**Estimated effort**: 3 days focused.
**Blocking**: testability (mock storage for fast unit tests), pluggable backends.
**Author**: 2026-05-21.

## Motivation

`Blockchain` currently holds `storage: Option<Arc<Storage>>` where
`Storage` is a concrete struct over redb. Three pains follow:

1. **Unit tests pay redb open/close costs.** Every test that touches
   storage opens a real redb file in `tempfile::tempdir()`. On a fast
   SSD this is OK; in CI on shared runners it noticeably slows the
   suite.
2. **Backends are hardcoded.** Swapping in a sled fallback, an in-memory
   `BTreeMap`, or an async S3-backed archival store requires touching
   every call site.
3. **Mocking failure paths is impossible.** "What if `put_block` fails
   on disk-full?" — there's no way to inject that without simulating a
   full filesystem.

`BlockStoreCursor` (refactor #28) makes this worse if not solved: it
takes `Arc<Storage>` directly, freezing the choice.

## Goal (P0)

Introduce a `PersistenceBackend` trait covering the API surface
`Blockchain` actually uses, with `Storage` (redb) and `InMemoryBackend`
(BTreeMap) as the two initial impls.

## Goal (P1)

Push `BlockStoreCursor::new` (#28) and every other consumer to
`Arc<dyn PersistenceBackend>`.

## Non-goals

- Async trait. Persistence stays sync from the caller's point of view;
  the async-persistence layer already inside `Blockchain` is a separate
  concern.
- Multi-tenant / namespacing. One backend = one chain.

## Surface

Grep `self.storage` in `chain.rs` to enumerate methods used. The actual
list (as of 2026-05-21):

```rust
pub trait PersistenceBackend: Send + Sync {
    // Block I/O
    fn put_block(&self, block: &Block) -> Result<(), StorageError>;
    fn get_block(&self, height: u64) -> Result<Option<Block>, StorageError>;
    fn prune_blocks_below(&self, retain_from: u64) -> Result<usize, StorageError>;
    fn get_height(&self) -> Result<Option<u64>, StorageError>;

    // Account / state
    fn put_account(&self, addr: &[u8], state: &AccountState) -> Result<(), StorageError>;
    fn get_account(&self, addr: &[u8]) -> Result<Option<AccountState>, StorageError>;
    fn get_all_accounts(&self) -> Result<Vec<(Vec<u8>, AccountState)>, StorageError>;

    // Contract / EVM state
    fn put_contract(&self, addr: &[u8], state: &ContractState) -> Result<(), StorageError>;
    fn get_contract(&self, addr: &[u8]) -> Result<Option<ContractState>, StorageError>;

    // Receipts + logs
    fn put_receipt(&self, tx_hash: &[u8], receipt: &Receipt) -> Result<(), StorageError>;
    fn get_receipt(&self, tx_hash: &[u8]) -> Result<Option<Receipt>, StorageError>;

    // Pending
    fn replace_pending_transactions(&self, txs: &[Transaction]) -> Result<(), StorageError>;
    fn get_all_pending_transactions_compat(&self, chain_id: &str) -> Result<Vec<Transaction>, StorageError>;

    // Slashing / epochs / governance / token registry
    fn put_evidence(&self, ev: &EquivocationEvidence) -> Result<(), StorageError>;
    fn put_epoch_snapshot(&self, h: u64, snap: &EpochSnapshot) -> Result<(), StorageError>;
    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError>;
    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    // Snapshot delivery
    fn put_snapshot_manifest(&self, height: u64, m: &SnapshotManifest) -> Result<(), StorageError>;
    fn get_snapshot_manifest(&self, height: u64) -> Result<Option<SnapshotManifest>, StorageError>;
    fn put_snapshot_chunk(&self, height: u64, index: usize, c: &StateChunk) -> Result<(), StorageError>;

    // Bookkeeping
    fn flush(&self) -> Result<(), StorageError>;
}
```

(Trim the surface as we go — a method only earns a slot in the trait if
`chain.rs` calls it.)

## Phases

### Phase A — Define the trait (0.5 day)

1. Create `src/storage/backend.rs`.
2. Move `Storage::open/put_block/get_block/...` signatures into the
   trait.
3. Have `Storage` impl the trait. Keep the concrete type alias
   `pub type RedbStorage = Storage;` so external callers don't break.

### Phase B — In-memory backend (0.5 day)

1. Create `src/storage/in_memory.rs` with `InMemoryBackend` using
   `BTreeMap<u64, Vec<u8>>` for each "table".
2. Implement `PersistenceBackend` over those maps.
3. Test it: every method round-trips correctly.

### Phase C — Migrate `Blockchain` to `dyn PersistenceBackend` (1 day)

1. Change `Blockchain::storage` from `Option<Arc<Storage>>` to
   `Option<Arc<dyn PersistenceBackend>>`.
2. Update every constructor and test fixture.
3. Update `BlockStoreCursor::new` to take `Arc<dyn PersistenceBackend>`.

### Phase D — Migrate the test fixtures (0.5 day)

1. Tests that don't care about persistence semantics use
   `InMemoryBackend` (no tmpdir, no fsync).
2. Tests specifically validating redb behavior (storage/mod.rs,
   block_store) keep using `Storage::open`.
3. Measure suite speed: target -30 % on cold runs.

### Phase E — Optional sled/S3 prototype (0.5 day)

Sketch one extra impl (sled or S3) to prove the abstraction holds
beyond two backends. **Don't merge unless we actually need it** — the
goal of the trait is testability, not premature multi-backend support.

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Trait surface grows organically and becomes hard to mock | Medium | Pre-write the trait from `grep self.storage` and refuse to add methods without a comment justifying why |
| `Send + Sync + 'static` bounds force `Mutex` everywhere | Medium | The existing `Arc<Storage>` is already `Send + Sync`; trait inherits |
| Async-persistence layer needs to specialize per backend | Low | Keep async layer at the chain-level, not in the trait |

## Acceptance criteria

1. `grep "Arc<Storage>" src/` returns 0 hits outside `storage/`.
2. `InMemoryBackend` passes every test that `Storage` passes.
3. Suite cold time drops by ≥20 %.
4. `BlockStoreCursor::new` takes `Arc<dyn PersistenceBackend>`.
