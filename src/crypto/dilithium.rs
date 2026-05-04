//! Post-quantum signatures (FIPS-204 ML-DSA-87).
//!
//! Backed by the pure-Rust `ml-dsa` crate (`=0.1.0-rc.9`), the same crate
//! and version pinned by `sdk/wasm/curs3d-wallet-wasm`. Because both sides
//! agree on the FIPS-204 finalized scheme, a signature produced in the
//! browser wallet verifies on the node byte-for-byte.
//!
//! The public API surface is intentionally identical to what the rest of
//! the codebase used to consume from the previous `pqcrypto-dilithium`
//! wrapper:
//!
//! * `KeyPair { public_key: Vec<u8>, secret_key: Vec<u8> }` with
//!   `generate()`, `sign(&self, msg) -> Signature`, and `public_key_hex()`.
//! * `Signature(pub Vec<u8>)` tuple struct — the bincode discriminant is
//!   the inner byte vector exactly, so on-disk and on-the-wire payloads
//!   stay shape-compatible.
//! * `verify(message, &Signature, &public_key) -> bool`.
//!
//! Sizes (FIPS-204 ML-DSA-87, NIST level 5):
//! * `public_key`: 2592 bytes
//! * `secret_key` (we store the 32-byte seed, expanded on-demand): 32 bytes
//! * `Signature.0`: 4627 bytes

use ml_dsa::{
    B32, EncodedSignature, EncodedVerifyingKey, KeyGen, MlDsa87, Signature as MlDsaSignature,
    SigningKey, VerifyingKey,
    signature::{Keypair as _, Signer as _, Verifier as _},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyPair {
    /// FIPS-204 verifying-key encoding (2592 bytes).
    pub public_key: Vec<u8>,
    /// 32-byte ML-DSA seed (`xi` in FIPS-204 §6.1). The expanded signing key
    /// is rederived on every signature; storing only the seed keeps wallet
    /// files small and matches the encoded private-key shape used by the
    /// browser wallet (`sdk/wasm/curs3d-wallet-wasm`).
    pub secret_key: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Signature(pub Vec<u8>);

impl KeyPair {
    pub fn generate() -> Self {
        // Pull a 32-byte seed from the OS RNG and feed it deterministically
        // into ML-DSA-87 keygen. We don't go through `MlDsa87::key_gen()`
        // (which takes an `&mut impl rand_core::CryptoRngCore`) because the
        // `rand_core` versions used by `rand 0.8` and `ml-dsa 0.1.0-rc.9`
        // disagree; the seeded path is the canonical FIPS-204 entry point
        // anyway and matches the wasm wallet exactly.
        let mut seed_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed_bytes);
        let mut seed = B32::default();
        seed.copy_from_slice(&seed_bytes);
        let sk = MlDsa87::from_seed(&seed);
        let vk = sk.verifying_key();
        let pk_enc: EncodedVerifyingKey<MlDsa87> = vk.encode();

        KeyPair {
            public_key: pk_enc.as_slice().to_vec(),
            secret_key: seed_bytes.to_vec(),
        }
    }

    /// Re-derive the expanded signing key from the stored 32-byte seed.
    fn signing_key(&self) -> Result<SigningKey<MlDsa87>, &'static str> {
        if self.secret_key.len() != 32 {
            return Err("secret key must be 32 bytes (ML-DSA seed)");
        }
        let mut seed = B32::default();
        seed.copy_from_slice(&self.secret_key);
        Ok(MlDsa87::from_seed(&seed))
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        let sk = self.signing_key().expect("invalid secret key");
        let sig: MlDsaSignature<MlDsa87> = sk.sign(message);
        let enc: EncodedSignature<MlDsa87> = sig.encode();
        Signature(enc.as_slice().to_vec())
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(&self.public_key)
    }
}

pub fn verify(message: &[u8], signature: &Signature, public_key: &[u8]) -> bool {
    let pk_arr: &EncodedVerifyingKey<MlDsa87> = match public_key.try_into() {
        Ok(arr) => arr,
        Err(_) => return false,
    };
    let sig_arr: &EncodedSignature<MlDsa87> = match signature.0.as_slice().try_into() {
        Ok(arr) => arr,
        Err(_) => return false,
    };
    let vk = VerifyingKey::<MlDsa87>::decode(pk_arr);
    let sig = match MlDsaSignature::<MlDsa87>::decode(sig_arr) {
        Some(s) => s,
        None => return false,
    };
    vk.verify(message, &sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_and_verify() {
        let kp = KeyPair::generate();
        let msg = b"CURS3D quantum-resistant blockchain";
        let sig = kp.sign(msg);
        assert!(verify(msg, &sig, &kp.public_key));
    }

    #[test]
    fn test_invalid_signature() {
        let kp = KeyPair::generate();
        let kp2 = KeyPair::generate();
        let msg = b"test message";
        let sig = kp.sign(msg);
        assert!(!verify(msg, &sig, &kp2.public_key));
    }

    #[test]
    fn test_ml_dsa_sizes_match_fips204_l5() {
        // ML-DSA-87 (FIPS-204 NIST level 5): pk = 2592, sig = 4627. Seed = 32.
        let kp = KeyPair::generate();
        assert_eq!(kp.public_key.len(), 2592);
        assert_eq!(kp.secret_key.len(), 32);
        let sig = kp.sign(b"size check");
        assert_eq!(sig.0.len(), 4627);
    }

    #[test]
    fn test_address_derivation_stable() {
        // The same KeyPair must always derive the same address bytes, and
        // the same public key reloaded into a fresh KeyPair must, too.
        let kp = KeyPair::generate();
        let a = crate::crypto::hash::address_bytes_from_public_key(&kp.public_key);
        let b = crate::crypto::hash::address_bytes_from_public_key(&kp.public_key);
        assert_eq!(a, b);

        let reloaded = KeyPair {
            public_key: kp.public_key.clone(),
            secret_key: kp.secret_key.clone(),
        };
        let c = crate::crypto::hash::address_bytes_from_public_key(&reloaded.public_key);
        assert_eq!(a, c);
    }

    /// Sanity check: signatures produced via the bare `ml-dsa` API verify
    /// through this wrapper. If the wasm crate or this wrapper drift in
    /// their `ml-dsa` setup, this test fails fast.
    ///
    /// The wasm wallet generates keys exactly the same way (32-byte seed
    /// fed into `MlDsa87::from_seed`, then `sign(msg)`), so this also
    /// stands in for a wasm-interop test on the native side.
    #[test]
    fn test_wasm_interop() {
        // Generate a keypair with `ml-dsa` directly — bypassing our wrapper.
        let mut seed_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed_bytes);
        let mut seed = B32::default();
        seed.copy_from_slice(&seed_bytes);
        let sk_direct = MlDsa87::from_seed(&seed);
        let vk_direct = sk_direct.verifying_key();
        let pk_bytes = vk_direct.encode().as_slice().to_vec();

        let msg = b"interop: signed by raw ml-dsa, verified by our wrapper";
        let sig_direct: MlDsaSignature<MlDsa87> = sk_direct.sign(msg);
        let sig_bytes = sig_direct.encode().as_slice().to_vec();

        // Verify through our wrapper.
        let sig = Signature(sig_bytes);
        assert!(verify(msg, &sig, &pk_bytes));

        // And the inverse: keys generated through our wrapper must produce
        // signatures decodable by the bare ml-dsa Verifier.
        let kp = KeyPair {
            public_key: pk_bytes.clone(),
            secret_key: seed_bytes.to_vec(),
        };
        let our_sig = kp.sign(msg);
        let our_sig_arr: &EncodedSignature<MlDsa87> =
            our_sig.0.as_slice().try_into().expect("sig size matches");
        let parsed = MlDsaSignature::<MlDsa87>::decode(our_sig_arr).expect("decode");
        let pk_arr: &EncodedVerifyingKey<MlDsa87> =
            pk_bytes.as_slice().try_into().expect("pk size matches");
        let vk = VerifyingKey::<MlDsa87>::decode(pk_arr);
        assert!(vk.verify(msg, &parsed).is_ok());
    }
}
