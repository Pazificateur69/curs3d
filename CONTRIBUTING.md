# Contributing to CURS3D

Thanks for looking at the project. This guide is the entry point for anyone
who wants to file a bug, propose a change, or build on top of the chain.

CURS3D is currently a solo-maintained Layer 1 testnet (post-quantum native
layer with an EVM-compatible side). Bus factor is genuinely 1, which means
two things: (1) every contribution genuinely helps, and (2) review may take
a few days — please don't take silence as rejection.

## Ground rules

1. **No private code, no obfuscation.** Everything that runs in a validator
   is in this repo. If you propose a change, it must be reviewable.
2. **Conservative cryptography.** Stick to FIPS-204 ML-DSA-87 for native
   signatures and secp256k1 for EVM. Custom schemes will be rejected.
3. **No surveillance, no backdoors, no telemetry.** Patches that add
   "phone home" behavior are out of scope, even opt-in.
4. **Be patient with the maintainer.** This is a part-time project and
   review cycles are measured in days, not minutes.

## Reporting a bug

Two channels, equally valid:

- **Public bug** (no exploit): open a GitHub issue on
  [`Pazificateur69/curs3d`](https://github.com/Pazificateur69/curs3d/issues).
- **Security vulnerability**: do **not** open a public issue. Use either
  a [private GitHub Security Advisory](https://github.com/Pazificateur69/curs3d/security/advisories)
  (preferred) or PGP-encrypted email to `security@curs3d.fr`. See
  [website/.well-known/security.txt](website/.well-known/security.txt) for
  the PGP fingerprint and `website/.well-known/security-pgp.asc` for the
  public key.

A good bug report contains:

- Steps to reproduce (ideally with a minimal test or a curl command)
- Expected vs observed behavior
- Commit hash (`git rev-parse HEAD`) and OS/arch
- Any relevant log excerpts (redact secrets first)

## Setting up locally

Required toolchain:

```bash
rustup install nightly --profile minimal
rustup target add wasm32-unknown-unknown  # optional, for native contracts SDK
brew install binaryen                     # optional, for wasm-opt
```

Build and test:

```bash
RUSTUP_TOOLCHAIN=nightly cargo build --release
RUSTUP_TOOLCHAIN=nightly cargo test --lib       # 211+ unit tests
RUSTUP_TOOLCHAIN=nightly cargo clippy --all-targets -- -D warnings
RUSTUP_TOOLCHAIN=nightly cargo fmt --all --check
```

Run a single-node localnet:

```bash
./deploy/scripts/init-localnet.sh
```

Run two nodes locally (more useful for consensus/gossip work):

```bash
# Terminal 1
cargo run --release -- node --port 4337 --data-dir /tmp/curs3d-n1
# Terminal 2
cargo run --release -- node --port 4338 --data-dir /tmp/curs3d-n2 \
  --bootnode /ip4/127.0.0.1/tcp/4337
```

## Proposing a change

1. **Open an issue first** for anything non-trivial. A 10-minute discussion
   prevents weeks of wasted work.
2. **Fork + branch.** Branch names: `fix/short-desc`, `feat/short-desc`,
   `refactor/short-desc`, `docs/short-desc`.
3. **Keep PRs focused.** One logical change per PR; mass refactors are
   reviewed slowly because they're risky.
4. **Tests required for behavioral changes.** No test = no merge. For
   consensus/crypto/storage code, add property-based tests via `proptest`
   when the input space is large.
5. **Document the "why".** Inline comments explain *why* a non-obvious
   choice was made, not *what* the code does (the code says what).
6. **No `unwrap()` in hot paths.** Use `expect("invariant: ...")` with a
   reason, or propagate `Result`. `unwrap()` in `#[test]` code is fine.
7. **Run the full CI locally before pushing**: `cargo test --lib`,
   `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all --check`,
   `cargo audit --deny unsound --deny yanked`.

## Areas that genuinely need help

These are the highest-leverage open items as of 2026-05-21 (see also
`docs/council-log.md` for the rationale):

- **Refactor `Blockchain::blocks: Vec<Block>` to a paginated block store**
  backed by `redb` with an LRU in-memory cache. Currently the chain holds
  every block in RAM, which OOMs validators after ~8500 blocks. Single
  highest-impact change you could make.
- **Split `core/chain.rs` (6800+ LOC)** into focused submodules
  (`apply.rs`, `produce.rs`, `finality.rs`, `mempool_api.rs`,
  `snapshot.rs`, etc.). Pure mechanical refactor, no logic change.
- **Chaos localnet 5–7 nodes in CI** with kill/partition/byzantine
  scenarios. We have unit tests but no multi-process integration tests.
- **VRF slot-leader**. Current scheduler is `sha3(height || prev_hash)`
  modulo cumulative stake — deterministic, predictable far ahead, DDoS
  surface. Migrate to a Verifiable Random Function (e.g. `schnorrkel`).
- **Property tests with `proptest`** on tx serialization, mempool
  ordering, fork choice, state-root determinism.

## Style

- Rust edition 2024, formatted with `rustfmt` (no custom config).
- Errors: `thiserror` enums for typed errors at module boundaries,
  `anyhow::Error` only for one-off scripts and tests.
- Logging: `tracing` (don't add `println!`/`eprintln!` to production paths).
- Naming: domain types use newtypes (`Address`, `BlockHash`, `TxHash`)
  rather than `[u8; 20]` / `Vec<u8>` — the migration to newtypes is in
  progress (task #38 in the project's internal task list).

## Commit messages

We use Conventional Commits-flavored messages but loose form:

```
<type>(<scope>): <short summary>

<optional longer body explaining motivation, trade-offs, references>
```

Types: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`,
`ci`, `deps`, `revert`. Scope is usually the module name (`core`,
`network`, `vm`, `consensus`, `storage`, `wallet`, `api`, `deploy`).

Examples from history:

```
fix(crypto): merkle 2nd-preimage resistance via leaf/node domain prefix
feat(network): pre-manifest chunk buffer + 50ms throttle for snapshots
refactor(chain): split apply/produce into separate submodules
```

## License

By submitting a contribution you agree your work is licensed under the
project's MIT license (see [LICENSE](LICENSE)).

## Questions

- General: GitHub Discussions or `discord.gg/curs3d` (when live)
- Security: see "Reporting a bug" above
- Direct: open an issue tagged `question`

Thanks.
