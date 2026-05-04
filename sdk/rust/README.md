# curs3d-contract — Rust SDK for CURS3D smart contracts

Write CURS3D smart contracts in Rust. Compiles to `wasm32-unknown-unknown`,
imports the CURS3D host ABI (`storage`, `logs`, `input`, `gas`), and ships with
five example contracts you can clone and adapt.

## Why this SDK

The CURS3D VM is a Wasmer-based WASM runtime with instruction-level fuel
metering and 11 host functions. Writing those bindings by hand is painful and
error-prone — this crate gives you safe, ergonomic Rust wrappers and a single
`entrypoint!()` macro that emits the symbol the chain expects (`curs3d_call`).

## Add to your contract

```toml
# Cargo.toml of your contract
[package]
name = "my-contract"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]    # WASM dynamic library

[dependencies]
curs3d-contract = { path = "<path-to-curs3d>/sdk/rust" }

[profile.release]
opt-level = "z"            # smallest binary
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

## Hello world

```rust
#![no_std]

use curs3d_contract::{entrypoint, log, storage};

entrypoint!(run);

fn run() {
    let next = storage::increment(b"count", 1);
    log::emit(b"tick", &next.to_le_bytes());
}
```

## Build

```bash
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
ls target/wasm32-unknown-unknown/release/*.wasm
```

## Deploy

Once you have your `.wasm`, deploy it with the CURS3D CLI:

```bash
curs3d send --to <wallet> --data path/to/contract.wasm --amount 0
# or POST /api/tx/submit with {"kind":"DeployContract", "data":"<hex>"}
```

The chain returns a deterministic contract address. Call the contract by
sending a `CallContract` transaction with `to = <contract_addr>` and the
input bytes you want to pass.

## API surface

| Module     | Function                          | Notes                                       |
|------------|-----------------------------------|---------------------------------------------|
| `storage`  | `get(&[u8]) -> Vec<u8>`           | Read raw bytes (empty when missing)         |
|            | `set(&[u8], &[u8])`               | Write raw bytes                             |
|            | `get_u64(&[u8]) -> u64`           | Convenience: little-endian u64              |
|            | `set_u64(&[u8], u64)`             | Convenience: little-endian u64              |
|            | `increment(&[u8], u64) -> u64`    | Atomic-ish counter increment                |
| `log`      | `emit(&[u8], &[u8])`              | Topic + data, surfaced via `/api/logs`      |
| `input`    | `len() -> usize`                  | Input payload length                        |
|            | `raw() -> Vec<u8>`                | Full payload                                |
|            | `selector() -> Option<u8>`        | First byte, useful for method dispatch      |
|            | `args() -> Vec<u8>`               | Everything after the selector               |
| `gas`      | `consume(u64)`                    | Manually charge gas                         |
|            | `loop_tick(u64)`                  | Required if your contract has any loop      |
| macro      | `entrypoint!(my_fn)`              | Emits the `curs3d_call` symbol the VM calls |

## Example contracts

```
sdk/rust/examples/
  counter/        # Simplest possible: increment a u64 + emit a log
  erc20-token/    # Fungible token (use the native CUR-20 registry in production)
  multisig/       # N-of-M wallet with proposal + voting
  nft/            # Minimal ERC-721 with mint/transfer/burn
  vesting/        # Linear unlock schedule, beneficiary credits via emitted logs
```

Each example builds independently:

```bash
cd sdk/rust/examples/counter
cargo build --release --target wasm32-unknown-unknown
```

## Limits to be aware of

- **No floats.** Floating-point ops are not deterministic across hardware.
- **Loops must call `gas::loop_tick(N)` per iteration**, otherwise the chain
  rejects the contract at deploy time as "unmetered loop".
- **No recursion past gas budget.** Each call has a strict gas envelope —
  use `gas::consume()` to charge for expensive work explicitly.
- **No imports beyond the `curs3d` module.** If your contract imports `env`,
  `wasi_snapshot_preview1`, or anything else, deployment fails.

## Roadmap (host ABI extensions)

- `caller() -> [u8; 20]` (so contracts can authenticate the sender)
- `block_height() -> u64`, `block_timestamp() -> u64`
- `transfer(to, amount)` for native CUR transfers from a contract
- Cross-contract calls (`call_contract(addr, input, gas) -> bytes`)

Once these are exposed, this SDK will gain the matching wrappers without
breaking the existing API.
