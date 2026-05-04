//! N-of-M multisig wallet contract.
//!
//! Owners are configured at deploy time via the `init` selector. Owners then
//! propose actions and vote. When an action collects >= `threshold` distinct
//! owner approvals it is marked executed and a `executed` log is emitted.
//!
//! Input encoding (selector + payload):
//!
//! | sel  | name      | payload                                                       |
//! |------|-----------|---------------------------------------------------------------|
//! | 0x01 | init      | threshold (u8) + count (u8) + count*20 bytes of owner addrs   |
//! | 0x02 | propose   | owner (20) + action_id (8) + payload (variable)               |
//! | 0x03 | approve   | owner (20) + action_id (8)                                    |
//!
//! State layout:
//!   `init`               -> [1] when initialized
//!   `t`                  -> threshold (u64 LE)
//!   `n`                  -> owner count (u64 LE)
//!   `o/<index>`          -> owner address (20 bytes)
//!   `o!/<addr>`          -> [1] when address is a known owner
//!   `p/<action_id>`      -> proposal payload
//!   `c/<action_id>`      -> approval count (u64 LE)
//!   `v/<aid>/<owner>`    -> [1] when owner has voted
//!   `e/<action_id>`      -> [1] when action executed

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
        0x02 => propose(body),
        0x03 => approve(body),
        _ => {}
    }
}

fn init(body: &[u8]) {
    if !storage::get(b"init").is_empty() {
        return;
    }
    let Some((threshold, rest)) = body.split_first() else { return };
    let Some((count, rest)) = rest.split_first() else { return };
    let count = *count as usize;
    if rest.len() < count * 20 || count == 0 || *threshold == 0 || *threshold as usize > count {
        return;
    }
    storage::set_u64(b"t", *threshold as u64);
    storage::set_u64(b"n", count as u64);
    for i in 0..count {
        let addr = &rest[i * 20..(i + 1) * 20];
        let mut idx_key = Vec::from(b"o/" as &[u8]);
        idx_key.extend_from_slice(&(i as u32).to_le_bytes());
        storage::set(&idx_key, addr);
        let mut owner_key = Vec::from(b"o!/" as &[u8]);
        owner_key.extend_from_slice(addr);
        storage::set(&owner_key, &[1]);
    }
    storage::set(b"init", &[1]);
    log::emit(b"init", body);
}

fn propose(body: &[u8]) {
    if body.len() < 28 {
        return;
    }
    let owner = &body[0..20];
    if !is_owner(owner) {
        return;
    }
    let aid = &body[20..28];
    let action_payload = &body[28..];

    let mut p_key = Vec::from(b"p/" as &[u8]);
    p_key.extend_from_slice(aid);
    if !storage::get(&p_key).is_empty() {
        return; // already proposed
    }
    storage::set(&p_key, action_payload);

    // Auto-count proposer's approval
    record_vote(aid, owner);
    log::emit(b"propose", body);
}

fn approve(body: &[u8]) {
    if body.len() < 28 {
        return;
    }
    let owner = &body[0..20];
    let aid = &body[20..28];
    if !is_owner(owner) {
        return;
    }
    record_vote(aid, owner);
}

fn record_vote(aid: &[u8], owner: &[u8]) {
    let mut vote_key = Vec::from(b"v/" as &[u8]);
    vote_key.extend_from_slice(aid);
    vote_key.push(b'/');
    vote_key.extend_from_slice(owner);
    if !storage::get(&vote_key).is_empty() {
        return;
    }
    storage::set(&vote_key, &[1]);
    let mut count_key = Vec::from(b"c/" as &[u8]);
    count_key.extend_from_slice(aid);
    let new_count = storage::get_u64(&count_key) + 1;
    storage::set_u64(&count_key, new_count);

    log::emit(b"approve", aid);

    let threshold = storage::get_u64(b"t");
    if new_count >= threshold {
        let mut e_key = Vec::from(b"e/" as &[u8]);
        e_key.extend_from_slice(aid);
        if storage::get(&e_key).is_empty() {
            storage::set(&e_key, &[1]);
            log::emit(b"executed", aid);
        }
    }
}

fn is_owner(addr: &[u8]) -> bool {
    let mut k = Vec::from(b"o!/" as &[u8]);
    k.extend_from_slice(addr);
    !storage::get(&k).is_empty()
}
