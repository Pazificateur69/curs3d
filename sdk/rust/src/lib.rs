//! # `curs3d-contract`
//!
//! Rust SDK for authoring CURS3D smart contracts. Contracts are `no_std` Rust
//! crates that compile to `wasm32-unknown-unknown` and import the CURS3D host
//! ABI (storage, logs, input, gas).
//!
//! ## Quickstart
//!
//! ```toml
//! # Cargo.toml of your contract crate
//! [package]
//! name = "my-contract"
//! version = "0.1.0"
//! edition = "2021"
//!
//! [lib]
//! crate-type = ["cdylib"]
//!
//! [dependencies]
//! curs3d-contract = { path = "../sdk/rust" }
//!
//! [profile.release]
//! opt-level = "z"
//! lto = true
//! codegen-units = 1
//! panic = "abort"
//! strip = true
//! ```
//!
//! ```rust,ignore
//! #![no_std]
//! #![no_main]
//!
//! use curs3d_contract::{entrypoint, gas, input, log, storage};
//!
//! entrypoint!(run);
//!
//! fn run() {
//!     gas::loop_tick(10); // any contract using a loop must call this
//!     let payload = input::raw();
//!     storage::set(b"latest", &payload);
//!     log::emit(b"updated", &payload);
//! }
//! ```
//!
//! Build with:
//!
//! ```bash
//! cargo build --release --target wasm32-unknown-unknown
//! # then deploy the .wasm file via the CURS3D CLI / API
//! ```

#![no_std]
#![cfg_attr(target_arch = "wasm32", allow(internal_features))]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

// ─── Host imports ────────────────────────────────────────────────────
//
// The CURS3D VM exposes its host ABI under the `curs3d` import module. We
// declare every host function here so that contracts can call them safely
// through the higher-level wrappers below.

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "curs3d")]
#[allow(dead_code)]
extern "C" {
    pub(crate) fn storage_get(key: i64) -> i64;
    pub(crate) fn storage_set(key: i64, value: i64);
    pub(crate) fn storage_read(key_ptr: i32, key_len: i32, dst_ptr: i32, dst_capacity: i32) -> i32;
    pub(crate) fn storage_write_bytes(
        key_ptr: i32,
        key_len: i32,
        value_ptr: i32,
        value_len: i32,
    ) -> i32;
    pub(crate) fn emit_log(topic: i64, data: i64);
    pub(crate) fn emit_log_bytes(
        topic_ptr: i32,
        topic_len: i32,
        data_ptr: i32,
        data_len: i32,
    ) -> i32;
    pub(crate) fn input() -> i64;
    pub(crate) fn input_len() -> i32;
    pub(crate) fn input_read(offset: i32, dst_ptr: i32, len: i32) -> i32;
    pub(crate) fn consume_gas(amount: i64);
    pub(crate) fn loop_tick(amount: i64);
}

// Stubs so the crate still compiles + tests on non-wasm targets (host machine).
// These will never be linked in real contracts, which always target wasm32.
#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_variables, dead_code)]
mod host_stubs {
    pub(crate) unsafe fn storage_get(_key: i64) -> i64 {
        0
    }
    pub(crate) unsafe fn storage_set(_key: i64, _value: i64) {}
    pub(crate) unsafe fn storage_read(
        _key_ptr: i32,
        _key_len: i32,
        _dst_ptr: i32,
        _dst_capacity: i32,
    ) -> i32 {
        0
    }
    pub(crate) unsafe fn storage_write_bytes(
        _key_ptr: i32,
        _key_len: i32,
        _value_ptr: i32,
        _value_len: i32,
    ) -> i32 {
        1
    }
    pub(crate) unsafe fn emit_log(_topic: i64, _data: i64) {}
    pub(crate) unsafe fn emit_log_bytes(
        _topic_ptr: i32,
        _topic_len: i32,
        _data_ptr: i32,
        _data_len: i32,
    ) -> i32 {
        1
    }
    pub(crate) unsafe fn input() -> i64 {
        0
    }
    pub(crate) unsafe fn input_len() -> i32 {
        0
    }
    pub(crate) unsafe fn input_read(_offset: i32, _dst_ptr: i32, _len: i32) -> i32 {
        0
    }
    pub(crate) unsafe fn consume_gas(_amount: i64) {}
    pub(crate) unsafe fn loop_tick(_amount: i64) {}
}

#[cfg(not(target_arch = "wasm32"))]
use host_stubs::*;

// ─── Storage ─────────────────────────────────────────────────────────

pub mod storage {
    //! Persistent contract storage (per-contract key/value map).

    use super::*;

    /// Read the value for `key`. Returns an empty Vec if the key is unset.
    pub fn get(key: &[u8]) -> Vec<u8> {
        let mut buf = vec![0u8; 1024];
        let written = unsafe {
            super::storage_read(
                key.as_ptr() as i32,
                key.len() as i32,
                buf.as_mut_ptr() as i32,
                buf.len() as i32,
            )
        };
        if written < 0 {
            return Vec::new();
        }
        buf.truncate(written as usize);
        buf
    }

    /// Write `value` under `key`.
    pub fn set(key: &[u8], value: &[u8]) {
        unsafe {
            super::storage_write_bytes(
                key.as_ptr() as i32,
                key.len() as i32,
                value.as_ptr() as i32,
                value.len() as i32,
            );
        }
    }

    /// Convenience: read a `u64` value (little-endian, 8 bytes). Returns 0 when missing.
    pub fn get_u64(key: &[u8]) -> u64 {
        let bytes = get(key);
        if bytes.len() < 8 {
            return 0;
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&bytes[..8]);
        u64::from_le_bytes(arr)
    }

    /// Convenience: write a `u64` value (little-endian, 8 bytes).
    pub fn set_u64(key: &[u8], value: u64) {
        set(key, &value.to_le_bytes());
    }

    /// Increment a `u64` counter and return the new value.
    pub fn increment(key: &[u8], delta: u64) -> u64 {
        let next = get_u64(key).saturating_add(delta);
        set_u64(key, next);
        next
    }
}

// ─── Logs ────────────────────────────────────────────────────────────

pub mod log {
    //! Emit log entries that are surfaced through the public `/api/logs` endpoint.

    /// Emit a log with a single topic and a data payload.
    pub fn emit(topic: &[u8], data: &[u8]) {
        unsafe {
            super::emit_log_bytes(
                topic.as_ptr() as i32,
                topic.len() as i32,
                data.as_ptr() as i32,
                data.len() as i32,
            );
        }
    }
}

// ─── Input ───────────────────────────────────────────────────────────

pub mod input {
    //! Access the call input bytes provided by the transaction sender.
    use super::*;

    /// Length of the input data in bytes.
    pub fn len() -> usize {
        unsafe { super::input_len().max(0) as usize }
    }

    /// Read the entire input payload.
    pub fn raw() -> Vec<u8> {
        let total = len();
        if total == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; total];
        let read = unsafe { super::input_read(0, buf.as_mut_ptr() as i32, buf.len() as i32) };
        if read < 0 {
            return Vec::new();
        }
        buf.truncate(read.max(0) as usize);
        buf
    }

    /// Read the first byte of the input as a method selector. Returns `None`
    /// when the input is empty.
    pub fn selector() -> Option<u8> {
        let raw = raw();
        raw.first().copied()
    }

    /// Slice of the input after byte 0 (typical "rest of args" after a selector).
    pub fn args() -> Vec<u8> {
        let mut raw = raw();
        if raw.is_empty() {
            raw
        } else {
            raw.drain(0..1);
            raw
        }
    }
}

// ─── Gas ─────────────────────────────────────────────────────────────

pub mod gas {
    //! Manual gas accounting helpers.

    /// Charge the given amount of gas. Aborts execution if the contract is out of gas.
    pub fn consume(amount: u64) {
        unsafe { super::consume_gas(amount as i64) }
    }

    /// Pay for one loop iteration. Contracts containing `loop`/`while`/`for` must
    /// import either this function or `consume_gas`, otherwise they are rejected
    /// at deployment time as "unmetered loop".
    #[inline]
    pub fn loop_tick(amount: u64) {
        unsafe { super::loop_tick(amount as i64) }
    }
}

// ─── Entrypoint helper ───────────────────────────────────────────────

/// Declare the contract entrypoint. The given function must be `fn() -> ()` or
/// `fn() -> i64`. The macro emits the `curs3d_call` symbol the VM expects.
///
/// ```rust,ignore
/// curs3d_contract::entrypoint!(run);
///
/// fn run() {
///     // contract logic
/// }
/// ```
#[macro_export]
macro_rules! entrypoint {
    ($name:ident) => {
        #[no_mangle]
        pub extern "C" fn curs3d_call() {
            $name();
        }
    };
    ($name:ident -> i64) => {
        #[no_mangle]
        pub extern "C" fn curs3d_call() -> i64 {
            $name()
        }
    };
}

// ─── Panic handler (wasm-only) ───────────────────────────────────────
//
// All wasm contracts need a panic handler. We provide one that traps the VM,
// which the host turns into a failed receipt. Contracts that want to ship
// their own handler can disable this with `default-features = false` on the
// `curs3d-contract` dependency.

#[cfg(all(target_arch = "wasm32", feature = "default-panic-handler"))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

// ─── Bump allocator (wasm-only) ──────────────────────────────────────
//
// Smart contracts execute in a fresh WASM instance per call, so an arena/bump
// allocator that never frees is the right choice: zero overhead, deterministic,
// and the entire heap goes away when the call returns. The arena lives in the
// contract's linear memory; growth is handled by Rust's built-in `memory.grow`.
//
// Contracts that need a real allocator can disable this feature and provide
// their own `#[global_allocator]`.

#[cfg(all(target_arch = "wasm32", feature = "default-allocator"))]
mod bump_allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr;

    // `__heap_base` is a symbol emitted by `wasm-ld` that points to the first
    // address past the static data section. Anchoring the bump allocator there
    // is the only way to guarantee we don't trample over static literals like
    // `b"hello"` that the contract embeds in its module.
    extern "C" {
        static __heap_base: u8;
    }

    pub struct BumpAllocator {
        offset: UnsafeCell<usize>,
    }

    // SAFETY: contracts are single-threaded inside the VM (no shared memory),
    // so an UnsafeCell is sufficient and Sync is sound.
    unsafe impl Sync for BumpAllocator {}

    unsafe impl GlobalAlloc for BumpAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let current = *self.offset.get();
            let start = if current == 0 {
                &__heap_base as *const u8 as usize
            } else {
                current
            };
            let aligned = (start + layout.align() - 1) & !(layout.align() - 1);
            let new_offset = aligned + layout.size();
            // Grow linear memory if we'd run past it (page = 64 KiB).
            let pages_now = core::arch::wasm32::memory_size(0) * 65536;
            if new_offset > pages_now {
                let needed_bytes = new_offset - pages_now;
                let needed_pages = needed_bytes.div_ceil(65536);
                if core::arch::wasm32::memory_grow(0, needed_pages) == usize::MAX {
                    return ptr::null_mut();
                }
            }
            *self.offset.get() = new_offset;
            aligned as *mut u8
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
            // Bump allocator: deallocation is a no-op. Memory is reclaimed when
            // the WASM instance is destroyed at the end of the call.
        }
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator {
        offset: UnsafeCell::new(0),
    };
}
