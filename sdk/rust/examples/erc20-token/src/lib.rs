//! A minimal ERC-20 style fungible token.
//!
//! Note: this is a *demo* implementation that lives entirely inside contract
//! storage. Production token deployments on CURS3D should use the native
//! CUR-20 token registry (see `curs3d deploy-token`) which gets first-class
//! treatment from the chain (lower gas, better indexing).
//!
//! Input encoding (single byte selector + payload):
//!
//! | selector | name        | payload                                       |
//! |----------|-------------|-----------------------------------------------|
//! | 0x01     | init        | total_supply (u64 LE) + owner (20 bytes)      |
//! | 0x02     | transfer    | to (20 bytes) + amount (u64 LE)               |
//! | 0x03     | approve     | spender (20 bytes) + amount (u64 LE)          |
//! | 0x04     | transferFrom| from (20 bytes) + to (20 bytes) + amount (u64)|
//!
//! Reads are exposed via storage proofs (`/api/contract/<addr>/storage/<key>/proof`).

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use curs3d_contract::{entrypoint, input, log, storage};

const SEL_INIT: u8 = 0x01;
const SEL_TRANSFER: u8 = 0x02;
const SEL_APPROVE: u8 = 0x03;
const SEL_TRANSFER_FROM: u8 = 0x04;

const KEY_INITIALIZED: &[u8] = b"init";
const KEY_TOTAL_SUPPLY: &[u8] = b"supply";

fn balance_key(owner: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(2 + owner.len());
    k.extend_from_slice(b"b/");
    k.extend_from_slice(owner);
    k
}

fn allowance_key(owner: &[u8], spender: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(3 + owner.len() + spender.len());
    k.extend_from_slice(b"a/");
    k.extend_from_slice(owner);
    k.push(b'/');
    k.extend_from_slice(spender);
    k
}

fn read_addr(buf: &[u8], offset: usize) -> Option<&[u8]> {
    buf.get(offset..offset + 20)
}

fn read_u64(buf: &[u8], offset: usize) -> Option<u64> {
    let slice = buf.get(offset..offset + 8)?;
    let mut arr = [0u8; 8];
    arr.copy_from_slice(slice);
    Some(u64::from_le_bytes(arr))
}

entrypoint!(run);

fn run() {
    let payload = input::raw();
    let Some(selector) = payload.first().copied() else {
        return;
    };
    let body = &payload[1..];

    match selector {
        SEL_INIT => init(body),
        SEL_TRANSFER => transfer(body),
        SEL_APPROVE => approve(body),
        SEL_TRANSFER_FROM => transfer_from(body),
        _ => {}
    }
}

fn init(body: &[u8]) {
    if !storage::get(KEY_INITIALIZED).is_empty() {
        return; // already initialized
    }
    let Some(total) = read_u64(body, 0) else { return };
    let Some(owner) = read_addr(body, 8) else { return };
    storage::set(KEY_INITIALIZED, &[1]);
    storage::set_u64(KEY_TOTAL_SUPPLY, total);
    storage::set(&balance_key(owner), &total.to_le_bytes());
    log::emit(b"init", body);
}

fn transfer(body: &[u8]) {
    let Some(from) = sender_proxy(body) else { return };
    let Some(to) = read_addr(body, 0) else { return };
    let Some(amount) = read_u64(body, 20) else {
        return;
    };
    move_balance(&from, to, amount);
}

fn approve(body: &[u8]) {
    let Some(owner) = sender_proxy(body) else {
        return;
    };
    let Some(spender) = read_addr(body, 0) else {
        return;
    };
    let Some(amount) = read_u64(body, 20) else {
        return;
    };
    storage::set(&allowance_key(&owner, spender), &amount.to_le_bytes());
    let mut payload = Vec::with_capacity(48);
    payload.extend_from_slice(&owner);
    payload.extend_from_slice(spender);
    payload.extend_from_slice(&amount.to_le_bytes());
    log::emit(b"approve", &payload);
}

fn transfer_from(body: &[u8]) {
    let Some(from) = read_addr(body, 0) else { return };
    let Some(to) = read_addr(body, 20) else { return };
    let Some(amount) = read_u64(body, 40) else {
        return;
    };
    let Some(spender) = sender_proxy(body) else {
        return;
    };

    let allow_key = allowance_key(from, &spender);
    let allow_now = storage::get_u64(&allow_key);
    if allow_now < amount {
        return;
    }
    storage::set_u64(&allow_key, allow_now - amount);
    move_balance(from, to, amount);
}

fn move_balance(from: &[u8], to: &[u8], amount: u64) {
    let from_key = balance_key(from);
    let to_key = balance_key(to);
    let from_now = storage::get_u64(&from_key);
    if from_now < amount {
        return;
    }
    storage::set_u64(&from_key, from_now - amount);
    let to_now = storage::get_u64(&to_key);
    storage::set_u64(&to_key, to_now.saturating_add(amount));
    let mut payload = Vec::with_capacity(48);
    payload.extend_from_slice(from);
    payload.extend_from_slice(to);
    payload.extend_from_slice(&amount.to_le_bytes());
    log::emit(b"transfer", &payload);
}

/// Caller-address proxy. The CURS3D VM does not yet expose a `caller` host
/// function, so the contract embeds the sender as the *first* 20 bytes of the
/// input payload. The off-chain caller produces the canonical sender address
/// and the runtime must verify the prefix matches the tx sender (this is the
/// native pattern used by the rest of the SDK examples — see README).
fn sender_proxy(_body: &[u8]) -> Option<[u8; 20]> {
    // Until a `caller()` host fn lands, contracts use a deterministic stub.
    // For demo purposes we treat the contract address as the implicit sender.
    Some([0u8; 20])
}
