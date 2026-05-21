# Refactor — split `network/mod.rs::run_with_chain()` handlers

**Status**: Design.
**Tracking task**: #30.
**Estimated effort**: 1 week.
**Blocking**: external audit, reorg risk, every future network change.

## Motivation

`src/network/mod.rs` is **3417 lines as of 2026-05-21**, of which the
single function `run_with_chain()` accounts for ~1500 LOC (the main
event loop with `tokio::select!` matching every gossipsub /
swarm-event /timer branch).

Pains:
- Adding a new message type requires editing the giant `select!`.
- Stack traces inside the handler are unintelligible — every closure
  inherits the outer scope.
- Tests must spin up the entire `tokio::select!` loop to exercise one
  handler.
- Auditors immediately bounce off the file.

## Goal (P0)

Reduce `network/mod.rs` to ≤1500 LOC. Extract every message handler
into `src/network/handlers/<topic>.rs`. The main loop becomes a thin
dispatcher.

## Proposed split

```
src/network/
├── mod.rs                   ← struct Node + new + run_with_chain (thin dispatcher only)   ~1500 LOC
├── peer_scoring.rs          ← PeerScorer (existing, no change)
├── rate_limiter.rs          ← PeerRateLimiter (existing, no change)
├── peerstore.rs             ← persistent peer cache + verified-addr hints
├── sync_gate.rs             ← startup sync gating + cold-sync logic
└── handlers/
    ├── mod.rs               ← handler trait + dispatch helper                              ~100 LOC
    ├── new_block.rs         ← handle_new_block + dedup + chain admit                       ~400 LOC
    ├── new_tx.rs            ← handle_new_transaction + mempool admit                       ~250 LOC
    ├── height_announce.rs   ← handle_height_announce + peer-address learning               ~300 LOC
    ├── request_blocks.rs    ← handle_request_blocks responder                              ~250 LOC
    ├── block_response.rs    ← handle_block_response receiver                               ~300 LOC
    ├── snapshot_request.rs  ← handle_snapshot_request responder                            ~200 LOC
    ├── snapshot_manifest.rs ← handle_snapshot_manifest receiver                            ~250 LOC
    ├── snapshot_chunk.rs    ← handle_snapshot_chunk receiver (incl. pre-manifest buffer)  ~400 LOC
    ├── finality_vote.rs     ← handle_finality_vote                                         ~200 LOC
    └── slashing.rs          ← handle_slashing_evidence                                     ~200 LOC
```

After the split, `run_with_chain()` is a `tokio::select!` of ~50 lines
that delegates every branch to a `handlers::<topic>::handle(...)` call.

## Handler signature

A consistent shape so every handler reads the same:

```rust
pub async fn handle(
    ctx: &mut HandlerCtx<'_>,
    msg: NewBlock,
) -> HandlerResult {
    // implementation
}

pub struct HandlerCtx<'a> {
    pub chain: &'a Arc<Mutex<Blockchain>>,
    pub node: &'a mut Node,
    pub peer_scorer: &'a mut PeerScorer,
    pub rate_limiter: &'a mut PeerRateLimiter,
    pub broadcast_queue: &'a mut VecDeque<NetworkMessage>,
    pub event_tx: &'a Option<broadcast::Sender<String>>,
    // … only fields the handler actually needs
}

pub enum HandlerResult {
    Accepted,
    Rejected(RejectReason),
    Deferred,    // e.g. buffered chunk; not yet processable
}
```

This makes handlers testable in isolation: build a `HandlerCtx` with
a real `Blockchain` (or mock), call `handle(ctx, msg)`, assert the
result + side effects.

## Phases

### Phase A — Inventory (1 day)

1. Grep `run_with_chain()` for every `NetworkMessage::*` match arm.
2. Build a spreadsheet mapping arm → target handler file.
3. Identify shared mutable state (the `HandlerCtx` fields).
4. Commit no code; just the inventory in a temporary `.md`.

### Phase B — Extract one handler as proof-of-concept (1 day)

`handlers::new_tx::handle` first — it's relatively self-contained
(mempool admit only, no chain mutation cascade).

1. Move the body into `handlers/new_tx.rs`.
2. Define `HandlerCtx` to satisfy what new_tx needs.
3. Replace the `select!` arm with a call to `handlers::new_tx::handle`.
4. CI green.

### Phase C — Extract every other handler (1 file per day, can parallelize)

Suggested order (easiest to hardest):
1. `slashing.rs`
2. `finality_vote.rs`
3. `height_announce.rs`
4. `request_blocks.rs`
5. `block_response.rs`
6. `new_block.rs`
7. `snapshot_request.rs`
8. `snapshot_manifest.rs`
9. `snapshot_chunk.rs` (largest)

### Phase D — Extract sync logic (1 day)

The startup sync gate + cold sync logic + verified-tip tracking lives
in run_with_chain today. Move to `sync_gate.rs`.

### Phase E — Tightening (0.5 day)

After everything is extracted:
- Mark `run_with_chain` < 200 LOC by composition.
- Add `//!` doc per handler explaining its single concern.

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| `HandlerCtx` becomes a 30-field god-struct | Medium | Refuse to add a field without justifying why; alternative is multiple smaller contexts |
| Async borrow checker fights vs. mutable state | High | Pre-design HandlerCtx with `&mut` fields, not `Arc<Mutex>`; the borrow checker is happiest at the call site |
| Performance regression from extra fn call | Low | Inline hint on hot handlers; tokio loops are far from CPU-bound |
| Splits expose hidden coupling (e.g. snapshot_manifest needs snapshot_chunk state) | Medium | Move shared state into `HandlerCtx` or a sibling module; never via globals |

## Acceptance criteria

1. `wc -l src/network/mod.rs` < 1500.
2. Each handler file < 500 LOC.
3. `cargo test --lib network` passes.
4. `cargo clippy --all-targets -- -D warnings` passes.
5. `run_with_chain()` body fits on one screen (≤ 80 lines).
