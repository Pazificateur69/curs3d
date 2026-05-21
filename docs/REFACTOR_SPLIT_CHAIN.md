# Refactor — split `core/chain.rs` into focused submodules

**Status**: Design.
**Tracking task**: #29.
**Estimated effort**: 1 week focused.
**Blocking**: external audit (god module is not auditable), every future
refactor in this module.

## Motivation

`src/core/chain.rs` is **6846 lines as of 2026-05-21**. The three AI
advisors (Claude / Gemini / Codex) unanimously flagged it as the #2
blocker after `Vec<Block>` — it bundles block production, block apply,
mempool admission, finality, slashing, snapshots, epoch settlement,
fork choice, governance dispatch, token dispatch, state-root
computation, fee market, and JSON serialization in a single struct
with ~200 methods.

Splitting:
- Makes diffs reviewable (no more "5000-line file changed" PRs).
- Lets clippy + rustc emit useful error context.
- Enables module-level documentation (`//!` headers) per concern.
- Required by any external auditor (Trail of Bits etc.) — they
  literally refuse to audit god modules.

## Goal (P0)

Reduce `chain.rs` to ≤1500 LOC. Move everything else into focused
submodules. Public API surface unchanged.

## Proposed split

```
src/core/chain/
├── mod.rs              ← re-exports + struct Blockchain (with fields only) + new() + getters    ~600 LOC
├── apply.rs            ← apply_block + apply_transaction + apply_*_kind dispatch                ~1500 LOC
├── produce.rs          ← create_block + block production gating                                  ~400 LOC
├── finality.rs         ← finality_tracker + slashing + epoch settlement                         ~800 LOC
├── mempool.rs          ← admit_transaction + enforce_mempool_limits + priority classes          ~700 LOC
├── snapshot.rs         ← snapshot generation + chunk streaming + apply_snapshot                 ~500 LOC
├── state_root.rs       ← compute_state_root_at_protocol + v5 merkle + v6 SMT dispatcher         ~400 LOC
├── replay.rs           ← boot-time block replay + state reconstruction                          ~300 LOC
├── reorg.rs            ← replace_blocks + invalidate_from + fork resolution                     ~300 LOC
└── proof.rs            ← state_proof generation + verification                                  ~400 LOC
```

Total target: ~6000 LOC across 10 files, average 600 LOC each.

The numbers above are rough — actual split happens by following the
existing visual section headers (`// ─── X ───`) in chain.rs.

## Phases

### Phase A — Inventory + section markers (1 day)

1. Add `// ─── SECTION: <name> ───` markers at every logical
   transition in `chain.rs`. Today the file already uses some — extend
   them everywhere.
2. Build a spreadsheet: section name → line range → target submodule.
3. Commit just the markers. CI green, zero behavior change.

### Phase B — Extract `apply.rs` (1-2 days)

1. `mkdir src/core/chain/` and move chain.rs to `src/core/chain/mod.rs`.
2. Cut the apply section into `apply.rs`.
3. Add `use crate::core::chain::*;` at the top of `apply.rs` to access
   types unchanged.
4. Re-export apply functions in `mod.rs` to preserve external paths.
5. CI green, full test suite passes.

### Phase C — Extract every other section (1 file per day)

Repeat phase B for each section. Each extraction is a single commit.

Suggested order, easiest to hardest:
1. `state_root.rs` (mostly pure fns)
2. `mempool.rs` (clear boundary)
3. `produce.rs`
4. `replay.rs`
5. `reorg.rs`
6. `proof.rs`
7. `snapshot.rs` (depends on multiple others)
8. `finality.rs` (largest, last)

### Phase D — Tightening (1 day)

After all sections are extracted:
1. Mark methods `pub(crate)` where they don't belong to the public API.
2. Move private helpers from `Blockchain::method` to module-level
   `fn` where they don't need `&self`.
3. Add `//!` doc comment per submodule.

## What NOT to do

- **No logic change during this refactor.** State-root semantics, fork
  choice, slashing — none of it gets modified. Refactor is mechanical
  move + visibility adjustment only.
- **No renaming of public types or methods.** External consumers
  (api/mod.rs, network/mod.rs, sdk/) must compile unchanged after each
  commit.
- **No new dependencies.**

## Risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| One submodule introduces a circular dep on another | High | Resolve via trait or by elevating shared types to mod.rs |
| Test fails mid-refactor and we can't isolate which extraction caused it | High | One extraction = one commit, each green. Bisect if needed. |
| Doctests on private fns disappear (private methods can't have doctests cross-module) | Low | Convert to `#[test]` unit tests where they fail to compile |

## Acceptance criteria

1. `wc -l src/core/chain/*.rs` shows no file > 1800 LOC.
2. `cargo test --lib` passes (full suite).
3. `cargo clippy --all-targets -- -D warnings` passes.
4. No file in `src/core/chain/` imports another `chain/` file with
   `use chain::*;` (explicit imports only — readability).
5. `mod.rs` only declares submodules + the `Blockchain` struct + its
   constructor. No business logic.
