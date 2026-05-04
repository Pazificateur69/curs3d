//! Minimal ERC-721 style NFT contract.
//!
//! Each token is a u64 id with a 20-byte owner address and an arbitrary metadata
//! payload set at mint time. Transfers move ownership.
//!
//! Input encoding:
//!
//! | sel  | name     | payload                                                  |
//! |------|----------|----------------------------------------------------------|
//! | 0x01 | mint     | to (20) + token_id (8) + metadata (variable, ≤256 bytes) |
//! | 0x02 | transfer | from (20) + to (20) + token_id (8)                       |
//! | 0x03 | burn     | owner (20) + token_id (8)                                |

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use curs3d_contract::{entrypoint, input, log, storage};

entrypoint!(run);

fn run() {
    let payload = input::raw();
    let Some(sel) = payload.first().copied() else { return };
    let body = &payload[1..];
    match sel {
        0x01 => mint(body),
        0x02 => transfer(body),
        0x03 => burn(body),
        _ => {}
    }
}

fn owner_key(token_id: &[u8]) -> Vec<u8> {
    let mut k = Vec::from(b"o/" as &[u8]);
    k.extend_from_slice(token_id);
    k
}

fn meta_key(token_id: &[u8]) -> Vec<u8> {
    let mut k = Vec::from(b"m/" as &[u8]);
    k.extend_from_slice(token_id);
    k
}

fn mint(body: &[u8]) {
    if body.len() < 28 {
        return;
    }
    let to = &body[0..20];
    let token_id = &body[20..28];
    let metadata = if body.len() > 28 && body.len() <= 28 + 256 {
        &body[28..]
    } else {
        &[][..]
    };
    let ok = owner_key(token_id);
    if !storage::get(&ok).is_empty() {
        return; // already minted
    }
    storage::set(&ok, to);
    storage::set(&meta_key(token_id), metadata);
    log::emit(b"mint", body);
}

fn transfer(body: &[u8]) {
    if body.len() < 48 {
        return;
    }
    let from = &body[0..20];
    let to = &body[20..40];
    let token_id = &body[40..48];
    let ok = owner_key(token_id);
    let current = storage::get(&ok);
    if current != from {
        return;
    }
    storage::set(&ok, to);
    log::emit(b"transfer", body);
}

fn burn(body: &[u8]) {
    if body.len() < 28 {
        return;
    }
    let owner = &body[0..20];
    let token_id = &body[20..28];
    let ok = owner_key(token_id);
    if storage::get(&ok) != owner {
        return;
    }
    storage::set(&ok, &[]);
    storage::set(&meta_key(token_id), &[]);
    log::emit(b"burn", body);
}
