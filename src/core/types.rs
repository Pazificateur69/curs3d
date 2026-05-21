//! Typed wrappers around the raw byte arrays used throughout the chain.
//!
//! Goal: replace `Vec<u8>` and `[u8; 20]` / `[u8; 32]` at API boundaries with
//! distinct types so that `Address`, `BlockHash`, and `TxHash` cannot be
//! confused at the type level. This catches an entire class of bugs at
//! compile time (e.g. passing a `BlockHash` where an `Address` is expected).
//!
//! **Scaffolding only**: these types are not yet wired into `Blockchain`,
//! `Transaction`, etc. They exist with `From<Vec<u8>>` and `Into<Vec<u8>>`
//! impls so migration can be incremental, one struct at a time.
//!
//! See task #38 in the project roadmap.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::crypto::hash::{self, ADDRESS_LEN};

/// 20-byte account address. Displayed as a checksummed `CUR...` string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Address(pub [u8; ADDRESS_LEN]);

impl Address {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != ADDRESS_LEN {
            return None;
        }
        let mut arr = [0u8; ADDRESS_LEN];
        arr.copy_from_slice(bytes);
        Some(Self(arr))
    }

    pub fn from_public_key(public_key: &[u8]) -> Self {
        let raw = hash::address_bytes_from_public_key(public_key);
        let mut arr = [0u8; ADDRESS_LEN];
        arr.copy_from_slice(&raw);
        Self(arr)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub fn to_checksum_string(&self) -> String {
        hash::checksum_address(&self.0)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_checksum_string())
    }
}

impl From<[u8; ADDRESS_LEN]> for Address {
    fn from(bytes: [u8; ADDRESS_LEN]) -> Self {
        Self(bytes)
    }
}

impl From<Address> for Vec<u8> {
    fn from(addr: Address) -> Self {
        addr.0.to_vec()
    }
}

impl TryFrom<&[u8]> for Address {
    type Error = TypeError;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        Self::from_bytes(value).ok_or(TypeError::WrongAddressLength {
            expected: ADDRESS_LEN,
            got: value.len(),
        })
    }
}

/// 32-byte block hash (double-SHA3 of a `BlockHeader`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockHash(pub [u8; 32]);

impl BlockHash {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 32 {
            return None;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Some(Self(arr))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 32]
    }
}

impl fmt::Display for BlockHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl From<[u8; 32]> for BlockHash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<BlockHash> for Vec<u8> {
    fn from(h: BlockHash) -> Self {
        h.0.to_vec()
    }
}

impl TryFrom<&[u8]> for BlockHash {
    type Error = TypeError;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        Self::from_bytes(value).ok_or(TypeError::WrongHashLength(value.len()))
    }
}

/// 32-byte transaction hash.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TxHash(pub [u8; 32]);

impl TxHash {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 32 {
            return None;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Some(Self(arr))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.0.to_vec()
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Display for TxHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl From<[u8; 32]> for TxHash {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<TxHash> for Vec<u8> {
    fn from(h: TxHash) -> Self {
        h.0.to_vec()
    }
}

impl TryFrom<&[u8]> for TxHash {
    type Error = TypeError;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        Self::from_bytes(value).ok_or(TypeError::WrongHashLength(value.len()))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TypeError {
    #[error("address must be {expected} bytes, got {got}")]
    WrongAddressLength { expected: usize, got: usize },
    #[error("hash must be 32 bytes, got {0}")]
    WrongHashLength(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_roundtrip() {
        let bytes = [0xabu8; ADDRESS_LEN];
        let a = Address::from(bytes);
        assert_eq!(a.as_bytes().len(), ADDRESS_LEN);
        let v: Vec<u8> = a.clone().into();
        assert_eq!(v.len(), ADDRESS_LEN);
        assert_eq!(Address::try_from(v.as_slice()).unwrap(), a);
    }

    #[test]
    fn address_wrong_length_rejected() {
        let short: &[u8] = &[1, 2, 3];
        assert!(matches!(
            Address::try_from(short),
            Err(TypeError::WrongAddressLength {
                expected: ADDRESS_LEN,
                got: 3
            })
        ));
    }

    #[test]
    fn block_and_tx_hashes_are_distinct_types() {
        // This wouldn't compile if BlockHash and TxHash were the same:
        let bh = BlockHash::from([1u8; 32]);
        let th = TxHash::from([1u8; 32]);
        // Their byte content is equal, but they are not interchangeable
        // at the type level — the value of this refactor.
        assert_eq!(bh.as_bytes(), th.as_bytes());
    }

    #[test]
    fn address_display_is_checksummed() {
        let bytes = [0x01u8; ADDRESS_LEN];
        let a = Address::from(bytes);
        let s = a.to_string();
        assert!(s.starts_with("CUR"));
        assert_eq!(s.len(), 3 + 40);
    }

    #[test]
    fn block_hash_zero() {
        let z = BlockHash::from([0u8; 32]);
        assert!(z.is_zero());
        let nz = BlockHash::from([1u8; 32]);
        assert!(!nz.is_zero());
    }

    #[test]
    fn address_from_public_key_matches_legacy_helper() {
        let pubkey = vec![7u8; 32];
        let a = Address::from_public_key(&pubkey);
        let legacy = hash::address_bytes_from_public_key(&pubkey);
        assert_eq!(a.as_bytes(), legacy.as_slice());
    }

    // ─── Property-based tests (task #33) ─────────────────────────────

    use proptest::prelude::*;

    proptest! {
        /// Any 20 bytes round-trip through Address.
        #[test]
        fn prop_address_roundtrip(bytes in proptest::array::uniform20(any::<u8>())) {
            let a = Address::from(bytes);
            let v: Vec<u8> = a.clone().into();
            let back = Address::try_from(v.as_slice()).unwrap();
            prop_assert_eq!(a, back);
        }

        /// Any byte slice of wrong length is rejected.
        #[test]
        fn prop_address_wrong_length_rejected(
            bytes in proptest::collection::vec(any::<u8>(), 0..40)
        ) {
            prop_assume!(bytes.len() != ADDRESS_LEN);
            prop_assert!(Address::try_from(bytes.as_slice()).is_err());
        }

        /// Any 32 bytes round-trip through BlockHash.
        #[test]
        fn prop_block_hash_roundtrip(bytes in proptest::array::uniform32(any::<u8>())) {
            let h = BlockHash::from(bytes);
            let v: Vec<u8> = h.clone().into();
            let back = BlockHash::try_from(v.as_slice()).unwrap();
            prop_assert_eq!(h, back);
        }

        /// Address derivation from public key is deterministic and yields
        /// the same bytes as the legacy helper.
        #[test]
        fn prop_address_from_pubkey_matches_legacy(
            pk in proptest::collection::vec(any::<u8>(), 0..256)
        ) {
            let typed = Address::from_public_key(&pk);
            let legacy = hash::address_bytes_from_public_key(&pk);
            prop_assert_eq!(typed.as_bytes(), legacy.as_slice());
        }

        /// Checksum display is always CUR + 40 hex chars.
        #[test]
        fn prop_address_display_format(bytes in proptest::array::uniform20(any::<u8>())) {
            let s = Address::from(bytes).to_string();
            prop_assert!(s.starts_with("CUR"));
            prop_assert_eq!(s.len(), 3 + 40);
        }
    }
}
