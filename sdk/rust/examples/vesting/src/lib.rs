//! Linear vesting schedule contract.
//!
//! Beneficiary, total amount, start_block and duration_blocks are configured at
//! init time. After init, any caller can poke the contract: the contract
//! computes how much of `total` should be unlocked at the *current_block*
//! (passed in via the input payload, since the VM does not yet expose a host
//! `block_height()` function) and emits a `release` log with the new
//! cumulative amount that the off-chain runtime can use to credit the
//! beneficiary.
//!
//! Input encoding:
//!
//! | sel  | name      | payload                                                            |
//! |------|-----------|--------------------------------------------------------------------|
//! | 0x01 | init      | beneficiary (20) + total (u64) + start_block (u64) + duration (u64)|
//! | 0x02 | release   | current_block (u64)                                                |
//! | 0x03 | view      | (none — emits a `state` log with the current claimable amount)     |

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use curs3d_contract::{entrypoint, log, storage};

entrypoint!(run);

fn run() {
    let payload = curs3d_contract::input::raw();
    let Some(sel) = payload.first().copied() else { return };
    let body = &payload[1..];
    match sel {
        0x01 => init(body),
        0x02 => release(body),
        0x03 => view_state(),
        _ => {}
    }
}

fn read_u64(buf: &[u8], offset: usize) -> Option<u64> {
    let slice = buf.get(offset..offset + 8)?;
    let mut arr = [0u8; 8];
    arr.copy_from_slice(slice);
    Some(u64::from_le_bytes(arr))
}

fn init(body: &[u8]) {
    if !storage::get(b"init").is_empty() {
        return;
    }
    if body.len() < 44 {
        return;
    }
    let beneficiary = &body[0..20];
    let Some(total) = read_u64(body, 20) else { return };
    let Some(start) = read_u64(body, 28) else { return };
    let Some(duration) = read_u64(body, 36) else {
        return;
    };
    if duration == 0 {
        return;
    }
    storage::set(b"init", &[1]);
    storage::set(b"ben", beneficiary);
    storage::set_u64(b"total", total);
    storage::set_u64(b"start", start);
    storage::set_u64(b"dur", duration);
    storage::set_u64(b"released", 0);
    log::emit(b"init", body);
}

fn release(body: &[u8]) {
    if body.len() < 8 || storage::get(b"init").is_empty() {
        return;
    }
    let Some(now) = read_u64(body, 0) else {
        return;
    };
    let total = storage::get_u64(b"total");
    let start = storage::get_u64(b"start");
    let dur = storage::get_u64(b"dur");
    let already = storage::get_u64(b"released");

    let unlocked = if now <= start {
        0
    } else if now >= start.saturating_add(dur) {
        total
    } else {
        // linear: total * (now - start) / duration
        let elapsed = now - start;
        ((total as u128) * (elapsed as u128) / (dur as u128)) as u64
    };

    if unlocked <= already {
        return;
    }
    let claimable = unlocked - already;
    storage::set_u64(b"released", unlocked);

    let ben = storage::get(b"ben");
    let mut payload = Vec::with_capacity(36);
    payload.extend_from_slice(&ben);
    payload.extend_from_slice(&claimable.to_le_bytes());
    payload.extend_from_slice(&unlocked.to_le_bytes());
    log::emit(b"release", &payload);
}

fn view_state() {
    let total = storage::get_u64(b"total");
    let released = storage::get_u64(b"released");
    let mut payload = Vec::with_capacity(16);
    payload.extend_from_slice(&total.to_le_bytes());
    payload.extend_from_slice(&released.to_le_bytes());
    log::emit(b"state", &payload);
}
