//! A minimal counter contract.
//!
//! Each call increments a stored u64 counter by 1 and emits a `tick` log event
//! containing the new value.

#![no_std]

use curs3d_contract::{entrypoint, log, storage};

entrypoint!(run);

fn run() {
    let next = storage::increment(b"count", 1);
    log::emit(b"tick", &next.to_le_bytes());
}
