//! CURS3D browser-side crypto bundle.
//!
//! Compiles to `wasm32-unknown-unknown` via `wasm-bindgen`. Exposes the
//! primitives a web wallet needs to manage CURS3D keys and sign transactions
//! entirely in the browser:
//!
//! * Post-quantum signatures: ML-DSA-87 (FIPS-204), pure-Rust, replaces the
//!   node's `pqcrypto-dilithium` (which can't compile to wasm32 because it
//!   wraps PQClean C code). Byte-for-byte interop with the existing node
//!   is **not** available — see README, "Interop". The intent is for the
//!   node to migrate to ML-DSA-87 alongside this crate.
//! * Argon2id KDF, parameters `m=64MB, t=3, p=4` — identical to the
//!   node's `wallet::hardened_argon2`.
//! * AES-256-GCM wallet encryption with the same on-disk JSON shape
//!   (`{salt, nonce, ciphertext}`) as the node's `EncryptedWallet`.
//! * SHA-3-256 address derivation with the EIP-55-style checksum used
//!   throughout the node.
//!
//! All errors raised across the wasm-bindgen boundary are `JsValue`s carrying
//! a human-readable string. Nothing inside is allowed to `panic!` reachable
//! from JS — every fallible operation returns `Result<T, JsValue>`.

#![cfg_attr(not(test), deny(unsafe_code))]
#![allow(clippy::missing_errors_doc)]

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use argon2::{Algorithm, Argon2, Params, Version};
use ml_dsa::{
    signature::{Keypair as _, Signer as _, Verifier as _},
    EncodedSignature, EncodedVerifyingKey, KeyGen, MlDsa87, Signature, SigningKey, VerifyingKey,
    B32,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Sha3_256};
use wasm_bindgen::prelude::*;
use zeroize::Zeroize;

// ============================================================================
// Constants — must mirror node-side `crypto/hash.rs` and `wallet/mod.rs`.
// ============================================================================

const ADDRESS_LEN: usize = 20;
const ADDRESS_DOMAIN: &[u8] = b"curs3d-address";

/// Argon2id memory cost (64 MiB), matches `wallet::hardened_argon2()`.
const KDF_M_COST_KIB: u32 = 65_536;
/// Argon2id time cost (3 iterations).
const KDF_T_COST: u32 = 3;
/// Argon2id parallelism (4 lanes).
const KDF_P_COST: u32 = 4;
/// Output key length (256-bit AES key).
const KDF_OUT_LEN: usize = 32;

/// Domain separator for transaction signing — must match node-side
/// `core/transaction.rs::signable_bytes`.
const TX_SIGN_DOMAIN: &[u8] = b"curs3d-tx-v1:";

// ============================================================================
// Helpers — hashing / address derivation.
// ============================================================================

fn sha3_256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha3_256::new();
    hasher.update(data);
    let out = hasher.finalize();
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&out);
    buf
}

fn sha3_hash_domain(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha3_256::new();
    hasher.update((domain.len() as u32).to_le_bytes());
    hasher.update(domain);
    for part in parts {
        hasher.update(part);
    }
    let out = hasher.finalize();
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&out);
    buf
}

fn address_bytes_from_public_key(public_key: &[u8]) -> [u8; ADDRESS_LEN] {
    let h = sha3_hash_domain(ADDRESS_DOMAIN, &[public_key]);
    let mut addr = [0u8; ADDRESS_LEN];
    addr.copy_from_slice(&h[..ADDRESS_LEN]);
    addr
}

/// EIP-55-style checksummed address — `CUR` + 40 hex chars, where each alpha
/// hex char is uppercased iff the corresponding nibble of `sha3(hex_lower)`
/// is `>= 8`. Mirrors `crypto::hash::checksum_address` on the node.
fn checksum_address(addr_bytes: &[u8]) -> String {
    let hex_lower = hex::encode(addr_bytes);
    let hash = sha3_256(hex_lower.as_bytes());
    let mut out = String::with_capacity(3 + hex_lower.len());
    out.push_str("CUR");
    for (i, c) in hex_lower.chars().enumerate() {
        let nibble = if i % 2 == 0 {
            hash[i / 2] >> 4
        } else {
            hash[i / 2] & 0x0f
        };
        if nibble >= 8 && c.is_ascii_alphabetic() {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Best-effort address bytes recovery for the JSON tx payload.
/// Accepts either `CUR<40-hex>` or raw `0x...` / hex.
fn parse_address(s: &str) -> Result<[u8; ADDRESS_LEN], String> {
    let core = s
        .strip_prefix("CUR")
        .or_else(|| s.strip_prefix("cur"))
        .or_else(|| s.strip_prefix("0x"))
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if core.len() != ADDRESS_LEN * 2 {
        return Err(format!(
            "address must be CUR + 40 hex chars (got {} chars)",
            core.len()
        ));
    }
    let bytes = hex::decode(core).map_err(|e| format!("invalid address hex: {e}"))?;
    let mut out = [0u8; ADDRESS_LEN];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Construct a `JsValue` carrying a human-readable message.
///
/// `JsValue::from_str` panics on non-wasm32 targets (it's only intended for
/// wasm runtime use), which would break `cargo test`. We wrap it so that
/// native test builds get a plain `JsValue::null()` placeholder while still
/// behaving correctly in the browser.
fn js_err(msg: impl Into<String>) -> JsValue {
    let s = msg.into();
    #[cfg(target_arch = "wasm32")]
    {
        JsValue::from_str(&s)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // On native (test) builds, wasm-bindgen's `JsValue::from_str` panics
        // because it has no JS context to live in. Use `JsValue::UNDEFINED`
        // as a placeholder and stash the message in a `JsValueError` attached
        // via thread-local for tests that want to inspect it.
        TEST_LAST_ERROR.with(|cell| {
            *cell.borrow_mut() = Some(s);
        });
        JsValue::UNDEFINED
    }
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    static TEST_LAST_ERROR: core::cell::RefCell<Option<String>> = const { core::cell::RefCell::new(None) };
}

/// Pull the last `js_err` message stashed on the current thread (test only).
#[cfg(all(not(target_arch = "wasm32"), test))]
fn last_err_message() -> Option<String> {
    TEST_LAST_ERROR.with(|cell| cell.borrow_mut().take())
}

// ============================================================================
// On-disk encrypted wallet — layout identical to the node's
// `wallet::EncryptedWallet`.
// ============================================================================

#[derive(Serialize, Deserialize)]
struct EncryptedWalletJson {
    salt: String,
    nonce: String,
    ciphertext: String,
    /// Schema version; the node currently writes `1`. Older files without
    /// this field are accepted (`#[serde(default)]` would be alternative;
    /// we choose strict acceptance here because the node always writes it).
    #[serde(default = "default_version")]
    version: u32,
}

fn default_version() -> u32 {
    1
}

/// Plaintext form serialized inside `ciphertext`. Mirrors the node's
/// `Wallet { keypair: KeyPair, address: String }` JSON exactly so wallets
/// round-trip between the CLI and the browser.
#[derive(Serialize, Deserialize)]
struct WalletPlaintext {
    keypair: KeyPairBytes,
    address: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct KeyPairBytes {
    public_key: Vec<u8>,
    secret_key: Vec<u8>,
}

// ============================================================================
// JSON shape of the `Transfer` transaction we POST to /api/tx/submit.
// Mirrors `core::transaction::Transaction` exactly (bincode-serialized
// payload, prefixed by `curs3d-tx-v1:` is what gets signed).
// ============================================================================

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
enum TransactionKind {
    Transfer,
    Stake,
    Unstake,
    Coinbase,
    DeployContract,
    CallContract,
    DeployToken,
    TokenTransfer,
    TokenApprove,
    TokenTransferFrom,
    SubmitProposal,
    GovernanceVote,
}

/// The exact payload we feed into bincode to produce the bytes that get
/// SHA-3 / signed. Field order MUST stay aligned with
/// `core::transaction::SignableTransaction` on the node.
#[derive(Serialize)]
struct SignableTransaction<'a> {
    chain_id: &'a str,
    kind: &'a TransactionKind,
    from: &'a [u8],
    sender_public_key: &'a [u8],
    to: &'a [u8],
    amount: u64,
    fee: u64,
    max_fee_per_gas: u64,
    max_priority_fee_per_gas: u64,
    nonce: u64,
    timestamp: i64,
    gas_limit: u64,
    data: &'a [u8],
}

/// JSON shape POSTed to `/api/tx/submit`. Matches `core::transaction::Transaction`.
#[derive(Serialize)]
struct TransactionJson<'a> {
    chain_id: &'a str,
    kind: TransactionKind,
    from: Vec<u8>,
    sender_public_key: &'a [u8],
    to: Vec<u8>,
    amount: u64,
    fee: u64,
    max_fee_per_gas: u64,
    max_priority_fee_per_gas: u64,
    nonce: u64,
    timestamp: i64,
    /// On the node this is `Option<dilithium::Signature>` where Signature is
    /// `pub struct Signature(pub Vec<u8>)`. With serde, that becomes
    /// `{"0": [..bytes..]}`. We mirror the bincode/JSON shape exactly via a
    /// tuple-struct-flavored representation.
    signature: Option<SignatureWire>,
    gas_limit: u64,
    data: Vec<u8>,
}

/// Mirrors the on-the-wire encoding of the node's
/// `Signature(pub Vec<u8>)` tuple struct under serde_json.
#[derive(Serialize)]
struct SignatureWire(Vec<u8>);

// ============================================================================
// Public API — KeyPair (ML-DSA-87)
// ============================================================================

/// A CURS3D wallet keypair (post-quantum, ML-DSA-87).
///
/// `public_key` is the 2592-byte FIPS-204 verifying-key encoding.
/// `secret_key` is the 32-byte ML-DSA seed (`xi` in FIPS-204 §6.1) — the
/// expanded signing key is rederived on demand. Storing the seed instead of
/// the expanded form keeps the wallet file small and matches the encoded
/// private-key format used by RustCrypto's `pkcs8` representation.
#[wasm_bindgen]
pub struct KeyPair {
    public_key: Vec<u8>,
    secret_key: Vec<u8>,
}

#[wasm_bindgen]
impl KeyPair {
    /// Generate a fresh keypair using the browser's `crypto.getRandomValues`.
    #[wasm_bindgen]
    pub fn generate() -> Self {
        // Pull a 32-byte ML-DSA seed from the OS RNG (which on wasm32 is
        // routed through `crypto.getRandomValues` by the `getrandom` crate).
        let mut seed_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut seed_bytes);
        let mut seed = B32::default();
        seed.copy_from_slice(&seed_bytes);
        let sk = MlDsa87::from_seed(&seed);
        let vk = sk.verifying_key();
        let pk_enc: EncodedVerifyingKey<MlDsa87> = vk.encode();
        let public_key = pk_enc.as_slice().to_vec();
        let secret_key = seed_bytes.to_vec();
        seed_bytes.zeroize();
        KeyPair {
            public_key,
            secret_key,
        }
    }

    /// CURS3D address (CUR + 40 hex chars, EIP-55 checksummed).
    #[wasm_bindgen]
    pub fn address(&self) -> String {
        checksum_address(&address_bytes_from_public_key(&self.public_key))
    }

    /// Hex-encoded public key bytes.
    #[wasm_bindgen(js_name = publicKeyHex)]
    pub fn public_key_hex(&self) -> String {
        hex::encode(&self.public_key)
    }

    /// Sign arbitrary bytes; returns the hex-encoded signature.
    #[wasm_bindgen]
    pub fn sign(&self, message: &[u8]) -> Result<String, JsValue> {
        let sig = self.sign_raw(message)?;
        Ok(hex::encode(sig))
    }

    /// Encrypt the keypair with a password and return JSON (matches the
    /// node's `EncryptedWallet`).
    #[wasm_bindgen(js_name = saveEncrypted)]
    pub fn save_encrypted(&self, password: &str) -> Result<String, JsValue> {
        encrypt_wallet(self, password).map_err(js_err)
    }

    /// Decrypt a JSON `EncryptedWallet` blob with a password.
    #[wasm_bindgen(js_name = loadEncrypted)]
    pub fn load_encrypted(json: &str, password: &str) -> Result<KeyPair, JsValue> {
        decrypt_wallet(json, password).map_err(js_err)
    }
}

impl KeyPair {
    fn signing_key(&self) -> Result<SigningKey<MlDsa87>, JsValue> {
        if self.secret_key.len() != 32 {
            return Err(js_err(format!(
                "secret key must be 32 bytes (was {})",
                self.secret_key.len()
            )));
        }
        let mut seed = B32::default();
        seed.copy_from_slice(&self.secret_key);
        Ok(MlDsa87::from_seed(&seed))
    }

    fn verifying_key(&self) -> Result<VerifyingKey<MlDsa87>, JsValue> {
        let arr: &EncodedVerifyingKey<MlDsa87> = self
            .public_key
            .as_slice()
            .try_into()
            .map_err(|_| js_err("invalid public key length"))?;
        Ok(VerifyingKey::<MlDsa87>::decode(arr))
    }

    /// Internal raw signer — produces the on-the-wire `Signature.0` bytes
    /// (FIPS-204 `pure` mode, `ctx = b""`).
    fn sign_raw(&self, message: &[u8]) -> Result<Vec<u8>, JsValue> {
        let sk = self.signing_key()?;
        let sig: Signature<MlDsa87> = sk.sign(message);
        let enc: EncodedSignature<MlDsa87> = sig.encode();
        Ok(enc.as_slice().to_vec())
    }

    fn from_bytes(public_key: Vec<u8>, secret_key: Vec<u8>) -> Result<Self, JsValue> {
        let kp = KeyPair {
            public_key,
            secret_key,
        };
        // Validate by parsing & re-deriving.
        let sk = kp.signing_key()?;
        let vk_derived = sk.verifying_key();
        let _ = kp.verifying_key()?;
        // Cross-check: the public key must match the one derived from the seed.
        if vk_derived.encode().as_slice() != kp.public_key.as_slice() {
            return Err(js_err("public key does not match secret key seed"));
        }
        Ok(kp)
    }
}

// ============================================================================
// verify_signature — free function on the wasm-bindgen surface.
// ============================================================================

/// Verify a signature against a public key and message.
///
/// Returns `false` for any malformed input rather than throwing, so callers
/// can use it as a clean predicate from JS.
#[wasm_bindgen(js_name = verifySignature)]
pub fn verify_signature(public_key_hex: &str, message: &[u8], signature_hex: &str) -> bool {
    let pk_bytes = match hex::decode(public_key_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let sig_bytes = match hex::decode(signature_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };
    let pk_arr: &EncodedVerifyingKey<MlDsa87> = match pk_bytes.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let sig_arr: &EncodedSignature<MlDsa87> = match sig_bytes.as_slice().try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let vk = VerifyingKey::<MlDsa87>::decode(pk_arr);
    let sig = match Signature::<MlDsa87>::decode(sig_arr) {
        Some(s) => s,
        None => return false,
    };
    vk.verify(message, &sig).is_ok()
}

// ============================================================================
// Wallet encryption / decryption
// ============================================================================

fn hardened_argon2() -> Result<Argon2<'static>, String> {
    let params = Params::new(KDF_M_COST_KIB, KDF_T_COST, KDF_P_COST, Some(KDF_OUT_LEN))
        .map_err(|e| format!("argon2 params: {e}"))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    let mut key = [0u8; 32];
    hardened_argon2()?
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| format!("argon2: {e}"))?;
    Ok(key)
}

fn encrypt_wallet(kp: &KeyPair, password: &str) -> Result<String, String> {
    let plaintext = WalletPlaintext {
        keypair: KeyPairBytes {
            public_key: kp.public_key.clone(),
            secret_key: kp.secret_key.clone(),
        },
        address: checksum_address(&address_bytes_from_public_key(&kp.public_key)),
    };
    let mut plaintext_bytes =
        serde_json::to_vec(&plaintext).map_err(|e| format!("serialize: {e}"))?;

    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let mut key = derive_key(password, &salt).inspect_err(|_| {
        plaintext_bytes.zeroize();
    })?;

    let cipher = match Aes256Gcm::new_from_slice(&key) {
        Ok(c) => c,
        Err(e) => {
            key.zeroize();
            plaintext_bytes.zeroize();
            return Err(format!("aes-gcm key: {e}"));
        }
    };

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let cipher_result = cipher
        .encrypt(nonce, plaintext_bytes.as_ref())
        .map_err(|e| format!("aes-gcm encrypt: {e}"));

    key.zeroize();
    plaintext_bytes.zeroize();

    let ciphertext = cipher_result?;

    let blob = EncryptedWalletJson {
        salt: hex::encode(salt),
        nonce: hex::encode(nonce_bytes),
        ciphertext: hex::encode(ciphertext),
        version: 1,
    };
    serde_json::to_string_pretty(&blob).map_err(|e| format!("serialize-out: {e}"))
}

fn decrypt_wallet(json: &str, password: &str) -> Result<KeyPair, String> {
    let blob: EncryptedWalletJson =
        serde_json::from_str(json).map_err(|e| format!("parse json: {e}"))?;
    let salt = hex::decode(&blob.salt).map_err(|e| format!("salt hex: {e}"))?;
    let nonce_bytes = hex::decode(&blob.nonce).map_err(|e| format!("nonce hex: {e}"))?;
    let ciphertext = hex::decode(&blob.ciphertext).map_err(|e| format!("ciphertext hex: {e}"))?;
    if nonce_bytes.len() != 12 {
        return Err(format!(
            "nonce must be 12 bytes (was {})",
            nonce_bytes.len()
        ));
    }

    let mut key = derive_key(password, &salt)?;
    let cipher = match Aes256Gcm::new_from_slice(&key) {
        Ok(c) => c,
        Err(e) => {
            key.zeroize();
            return Err(format!("aes-gcm key: {e}"));
        }
    };
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext_result = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| "wrong password".to_string());

    key.zeroize();

    let mut plaintext = plaintext_result?;
    let parsed: Result<WalletPlaintext, _> =
        serde_json::from_slice(&plaintext).map_err(|e| format!("decode wallet: {e}"));
    plaintext.zeroize();
    let parsed = parsed?;

    KeyPair::from_bytes(parsed.keypair.public_key, parsed.keypair.secret_key).map_err(|e| {
        e.as_string()
            .unwrap_or_else(|| "decode keypair".to_string())
    })
}

// ============================================================================
// build_transfer_tx — produces JSON ready to POST to /api/tx/submit.
// Bincode payload order MUST match `core::transaction::SignableTransaction`.
// ============================================================================

/// Build, sign, and serialize a CURS3D `Transfer` transaction.
#[wasm_bindgen(js_name = buildTransferTx)]
pub fn build_transfer_tx(
    keypair: &KeyPair,
    to_address: &str,
    amount_microtokens: u64,
    fee_microtokens: u64,
    nonce: u64,
) -> Result<String, JsValue> {
    let to = parse_address(to_address).map_err(js_err)?;
    build_transfer_tx_inner(
        keypair,
        &to,
        amount_microtokens,
        fee_microtokens,
        nonce,
        current_unix_timestamp(),
        "curs3d-public-testnet",
    )
    .map_err(js_err)
}

/// Variant of `buildTransferTx` that lets the caller control the chain id
/// and timestamp — useful for deterministic tests and for environments
/// (devnets, future mainnet) on a different chain id.
#[wasm_bindgen(js_name = buildTransferTxAdvanced)]
#[allow(clippy::too_many_arguments)]
pub fn build_transfer_tx_advanced(
    keypair: &KeyPair,
    to_address: &str,
    amount_microtokens: u64,
    fee_microtokens: u64,
    nonce: u64,
    timestamp: i64,
    chain_id: &str,
) -> Result<String, JsValue> {
    let to = parse_address(to_address).map_err(js_err)?;
    build_transfer_tx_inner(
        keypair,
        &to,
        amount_microtokens,
        fee_microtokens,
        nonce,
        timestamp,
        chain_id,
    )
    .map_err(js_err)
}

fn build_transfer_tx_inner(
    keypair: &KeyPair,
    to: &[u8; ADDRESS_LEN],
    amount: u64,
    fee: u64,
    nonce: u64,
    timestamp: i64,
    chain_id: &str,
) -> Result<String, String> {
    let from = address_bytes_from_public_key(&keypair.public_key);
    let kind = TransactionKind::Transfer;

    let signable = SignableTransaction {
        chain_id,
        kind: &kind,
        from: &from,
        sender_public_key: &keypair.public_key,
        to,
        amount,
        fee,
        max_fee_per_gas: fee,
        max_priority_fee_per_gas: fee,
        nonce,
        timestamp,
        gas_limit: 0,
        data: &[],
    };
    let mut prefixed = TX_SIGN_DOMAIN.to_vec();
    let payload = bincode::serialize(&signable).map_err(|e| format!("bincode signable: {e}"))?;
    prefixed.extend_from_slice(&payload);
    let sig_bytes = keypair
        .sign_raw(&prefixed)
        .map_err(|e| e.as_string().unwrap_or_else(|| "sign".to_string()))?;

    let tx = TransactionJson {
        chain_id,
        kind: TransactionKind::Transfer,
        from: from.to_vec(),
        sender_public_key: &keypair.public_key,
        to: to.to_vec(),
        amount,
        fee,
        max_fee_per_gas: fee,
        max_priority_fee_per_gas: fee,
        nonce,
        timestamp,
        signature: Some(SignatureWire(sig_bytes)),
        gas_limit: 0,
        data: Vec::new(),
    };
    serde_json::to_string(&tx).map_err(|e| format!("serialize tx: {e}"))
}

/// Wall-clock seconds since Unix epoch — works in the browser via
/// `Date.now()` (the `js-sys::Date::now` returns ms).
fn current_unix_timestamp() -> i64 {
    #[cfg(target_arch = "wasm32")]
    {
        let ms = js_sys::Date::now();
        (ms / 1000.0) as i64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

// ============================================================================
// Tests — run with plain `cargo test` (NOT the wasm-bindgen-test runner).
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keygen_sign_verify_roundtrip() {
        let kp = KeyPair::generate();
        let msg = b"CURS3D wasm signing roundtrip";
        let sig_hex = kp.sign(msg).expect("sign ok");
        assert!(verify_signature(&kp.public_key_hex(), msg, &sig_hex));
    }

    #[test]
    fn verify_rejects_tampered_message() {
        let kp = KeyPair::generate();
        let sig_hex = kp.sign(b"hello").unwrap();
        assert!(!verify_signature(
            &kp.public_key_hex(),
            b"goodbye",
            &sig_hex
        ));
    }

    #[test]
    fn verify_rejects_other_keypair() {
        let kp_a = KeyPair::generate();
        let kp_b = KeyPair::generate();
        let msg = b"signed by A";
        let sig = kp_a.sign(msg).unwrap();
        assert!(verify_signature(&kp_a.public_key_hex(), msg, &sig));
        assert!(!verify_signature(&kp_b.public_key_hex(), msg, &sig));
    }

    #[test]
    fn address_is_eip55_checksum() {
        let kp = KeyPair::generate();
        let addr = kp.address();
        assert!(addr.starts_with("CUR"));
        assert_eq!(addr.len(), 43); // "CUR" + 40 hex chars
                                    // Every alpha char must be either upper or lower per the SHA3 nibble rule
                                    // — round-trip through verify-style logic.
        let hex_part = addr.strip_prefix("CUR").unwrap();
        let lower = hex_part.to_ascii_lowercase();
        // Decode + recompute and assert equality.
        let bytes = hex::decode(&lower).unwrap();
        assert_eq!(checksum_address(&bytes), addr);
    }

    #[test]
    fn address_is_deterministic_from_public_key() {
        let kp = KeyPair::generate();
        let a = kp.address();
        let b = kp.address();
        assert_eq!(a, b);
        // Reload via from_bytes and re-derive
        let reload = KeyPair::from_bytes(kp.public_key.clone(), kp.secret_key.clone()).unwrap();
        assert_eq!(reload.address(), a);
    }

    #[test]
    fn encrypted_wallet_roundtrip() {
        let kp = KeyPair::generate();
        let json = kp.save_encrypted("correct horse battery staple").unwrap();
        let back = KeyPair::load_encrypted(&json, "correct horse battery staple").unwrap();
        assert_eq!(back.public_key, kp.public_key);
        assert_eq!(back.secret_key, kp.secret_key);
        assert_eq!(back.address(), kp.address());
    }

    #[test]
    fn encrypted_wallet_wrong_password() {
        let kp = KeyPair::generate();
        let json = kp.save_encrypted("right password").unwrap();
        // KeyPair doesn't implement Debug, so we can't use unwrap_err().
        match KeyPair::load_encrypted(&json, "wrong password") {
            Ok(_) => panic!("decrypt with wrong password unexpectedly succeeded"),
            Err(_) => {
                // On native test targets, `js_err` stashes the message on a
                // thread-local; assert that it carries the expected reason.
                let msg = last_err_message().unwrap_or_default();
                assert!(
                    msg.contains("wrong password"),
                    "expected 'wrong password', got: {msg}",
                );
            }
        }
    }

    #[test]
    fn encrypted_wallet_json_shape_matches_node() {
        // The browser-produced blob must parse as the node's
        // `EncryptedWallet` (salt/nonce/ciphertext hex strings + version).
        let kp = KeyPair::generate();
        let json = kp.save_encrypted("p").unwrap();
        let parsed: EncryptedWalletJson = serde_json::from_str(&json).unwrap();
        // Salt = 16 bytes -> 32 hex chars
        assert_eq!(parsed.salt.len(), 32);
        // Nonce = 12 bytes -> 24 hex chars
        assert_eq!(parsed.nonce.len(), 24);
        // Ciphertext non-empty
        assert!(!parsed.ciphertext.is_empty());
        assert_eq!(parsed.version, 1);
    }

    /// End-to-end: build a Transfer tx, then verify the signature embedded
    /// in the resulting JSON against the keypair's public key, exactly the
    /// way the node's `Transaction::verify_signature` does (re-bincode
    /// the signable payload, prepend the domain tag, run ML-DSA verify).
    #[test]
    fn transfer_tx_signature_verifies() {
        let kp = KeyPair::generate();
        let to_addr = "CUR0000000000000000000000000000000000000001";
        let json = build_transfer_tx_advanced(
            &kp,
            to_addr,
            12_345_678,
            1_000,
            7,
            1_700_000_000,
            "curs3d-public-testnet",
        )
        .expect("build_transfer_tx ok");

        // Re-parse the JSON, re-construct the signable bytes, verify.
        // serde flattens a single-field tuple struct (Signature(Vec<u8>) on the
        // node, SignatureWire(Vec<u8>) here) directly into the inner array,
        // so `signature` is a JSON array of integers, not an object.
        #[derive(Deserialize)]
        struct TxIn {
            chain_id: String,
            kind: String,
            from: Vec<u8>,
            sender_public_key: Vec<u8>,
            to: Vec<u8>,
            amount: u64,
            fee: u64,
            max_fee_per_gas: u64,
            max_priority_fee_per_gas: u64,
            nonce: u64,
            timestamp: i64,
            signature: Vec<u8>,
            gas_limit: u64,
            data: Vec<u8>,
        }

        let tx: TxIn = serde_json::from_str(&json).expect("parse");
        assert_eq!(tx.chain_id, "curs3d-public-testnet");
        assert_eq!(tx.kind, "Transfer");
        assert_eq!(tx.amount, 12_345_678);
        assert_eq!(tx.fee, 1_000);
        assert_eq!(tx.max_fee_per_gas, 1_000);
        assert_eq!(tx.max_priority_fee_per_gas, 1_000);
        assert_eq!(tx.nonce, 7);
        assert_eq!(tx.timestamp, 1_700_000_000);
        assert_eq!(tx.gas_limit, 0);
        assert!(tx.data.is_empty());
        assert_eq!(tx.from.len(), ADDRESS_LEN);
        assert_eq!(tx.to.len(), ADDRESS_LEN);
        assert_eq!(tx.sender_public_key, kp.public_key);

        // Recompute signable bytes — must use the SAME `SignableTransaction`
        // shape as the signer.
        let kind = TransactionKind::Transfer;
        let signable = SignableTransaction {
            chain_id: &tx.chain_id,
            kind: &kind,
            from: &tx.from,
            sender_public_key: &tx.sender_public_key,
            to: &tx.to,
            amount: tx.amount,
            fee: tx.fee,
            max_fee_per_gas: tx.max_fee_per_gas,
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas,
            nonce: tx.nonce,
            timestamp: tx.timestamp,
            gas_limit: tx.gas_limit,
            data: &tx.data,
        };
        let mut prefixed = TX_SIGN_DOMAIN.to_vec();
        prefixed.extend_from_slice(&bincode::serialize(&signable).unwrap());

        let sig_hex = hex::encode(&tx.signature);
        let pk_hex = hex::encode(&tx.sender_public_key);
        assert!(
            verify_signature(&pk_hex, &prefixed, &sig_hex),
            "signature must verify against the canonical signable bytes"
        );

        // And the embedded `from` address must be the SHA3-derived address.
        assert_eq!(tx.from, address_bytes_from_public_key(&kp.public_key));
    }

    #[test]
    fn parse_address_accepts_all_known_forms() {
        let raw_hex = "0123456789abcdef0123456789abcdef01234567";
        let cur = format!("CUR{}", raw_hex);
        let zero_x = format!("0x{}", raw_hex);
        let parsed_cur = parse_address(&cur).unwrap();
        let parsed_0x = parse_address(&zero_x).unwrap();
        let parsed_raw = parse_address(raw_hex).unwrap();
        assert_eq!(parsed_cur, parsed_0x);
        assert_eq!(parsed_cur, parsed_raw);
        assert_eq!(hex::encode(parsed_cur), raw_hex);
    }

    #[test]
    fn parse_address_rejects_short_input() {
        assert!(parse_address("CUR0123").is_err());
        assert!(parse_address("CURnothex0000000000000000000000000000000000").is_err());
    }

    #[test]
    fn ml_dsa_sizes_match_fips204_l5() {
        // ML-DSA-87 (FIPS-204 NIST level 5) public key = 2592, signature = 4627.
        let kp = KeyPair::generate();
        assert_eq!(kp.public_key.len(), 2592);
        let sig_hex = kp.sign(b"x").unwrap();
        assert_eq!(sig_hex.len() / 2, 4627);
    }
}
