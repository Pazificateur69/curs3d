use sha3::{Digest, Sha3_256};

pub const ADDRESS_LEN: usize = 20;

pub fn sha3_hash(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha3_256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

/// Domain-separated SHA-3 hash.
/// Prevents cross-layer collisions by prefixing with a unique domain tag.
/// Format: SHA3( len(domain) || domain || data_0 || data_1 || ... )
pub fn sha3_hash_domain(domain: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = Sha3_256::new();
    hasher.update((domain.len() as u32).to_le_bytes());
    hasher.update(domain);
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_vec()
}

pub fn double_hash(data: &[u8]) -> Vec<u8> {
    sha3_hash(&sha3_hash(data))
}

pub fn address_bytes_from_public_key(public_key: &[u8]) -> Vec<u8> {
    let hash = sha3_hash_domain(b"curs3d-address", &[public_key]);
    hash[..ADDRESS_LEN].to_vec()
}

pub fn address_bytes_from_data(data: &[u8]) -> Vec<u8> {
    let hash = sha3_hash_domain(b"curs3d-derived-address", &[data]);
    hash[..ADDRESS_LEN].to_vec()
}

pub fn address_string_from_public_key(public_key: &[u8]) -> String {
    let addr_bytes = address_bytes_from_public_key(public_key);
    checksum_address(&addr_bytes)
}

/// Compute a checksummed CUR address (EIP-55 style).
/// The hex characters are uppercased when the corresponding nibble of the
/// hash of the lowercase hex is >= 8, providing typo detection.
pub fn checksum_address(addr_bytes: &[u8]) -> String {
    let hex_lower = hex::encode(addr_bytes);
    let hash = sha3_hash(hex_lower.as_bytes());

    let mut checksummed = String::with_capacity(3 + hex_lower.len());
    checksummed.push_str("CUR");

    for (i, c) in hex_lower.chars().enumerate() {
        let hash_nibble = if i % 2 == 0 {
            hash[i / 2] >> 4
        } else {
            hash[i / 2] & 0x0f
        };
        if hash_nibble >= 8 && c.is_ascii_alphabetic() {
            checksummed.push(c.to_ascii_uppercase());
        } else {
            checksummed.push(c);
        }
    }

    checksummed
}

/// Verify a checksummed CUR address. Returns true if valid checksum or all-lowercase.
pub fn verify_checksum_address(address: &str) -> bool {
    let hex_part = address.strip_prefix("CUR").unwrap_or(address);
    if hex_part.len() != ADDRESS_LEN * 2 {
        return false;
    }

    // All-lowercase is always valid (no checksum applied)
    if hex_part == hex_part.to_ascii_lowercase() {
        return true;
    }

    // Verify checksum
    let addr_bytes = match hex::decode(hex_part.to_ascii_lowercase()) {
        Ok(b) if b.len() == ADDRESS_LEN => b,
        _ => return false,
    };

    let expected = checksum_address(&addr_bytes);
    let expected_hex = expected.strip_prefix("CUR").unwrap_or(&expected);
    hex_part == expected_hex
}

pub fn merkle_root(hashes: &[Vec<u8>]) -> Vec<u8> {
    if hashes.is_empty() {
        return sha3_hash(b"empty");
    }
    if hashes.len() == 1 {
        return hashes[0].clone();
    }

    let mut current_level = hashes.to_vec();

    while current_level.len() > 1 {
        let mut next_level = Vec::new();

        for chunk in current_level.chunks(2) {
            let mut combined = chunk[0].clone();
            if chunk.len() == 2 {
                combined.extend_from_slice(&chunk[1]);
            } else {
                combined.extend_from_slice(&chunk[0]);
            }
            next_level.push(sha3_hash(&combined));
        }
        current_level = next_level;
    }

    current_level.remove(0)
}

pub fn merkle_proof(hashes: &[Vec<u8>], index: usize) -> Vec<Vec<u8>> {
    if hashes.is_empty() || index >= hashes.len() {
        return Vec::new();
    }
    if hashes.len() == 1 {
        return Vec::new();
    }

    let mut proof = Vec::new();
    let mut current_level = hashes.to_vec();
    let mut current_index = index;

    while current_level.len() > 1 {
        let sibling_index = if current_index.is_multiple_of(2) {
            (current_index + 1).min(current_level.len() - 1)
        } else {
            current_index - 1
        };
        proof.push(current_level[sibling_index].clone());

        let mut next_level = Vec::new();
        for chunk in current_level.chunks(2) {
            let mut combined = chunk[0].clone();
            if chunk.len() == 2 {
                combined.extend_from_slice(&chunk[1]);
            } else {
                combined.extend_from_slice(&chunk[0]);
            }
            next_level.push(sha3_hash(&combined));
        }
        current_level = next_level;
        current_index /= 2;
    }

    proof
}

pub fn verify_merkle_proof(leaf_hash: &[u8], proof: &[Vec<u8>], index: usize, root: &[u8]) -> bool {
    if root.is_empty() {
        return false;
    }
    let mut current = leaf_hash.to_vec();
    let mut current_index = index;

    for sibling in proof {
        let mut combined = Vec::with_capacity(current.len() + sibling.len());
        if current_index.is_multiple_of(2) {
            combined.extend_from_slice(&current);
            combined.extend_from_slice(sibling);
        } else {
            combined.extend_from_slice(sibling);
            combined.extend_from_slice(&current);
        }
        current = sha3_hash(&combined);
        current_index /= 2;
    }

    current == root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha3_deterministic() {
        let h1 = sha3_hash(b"curs3d");
        let h2 = sha3_hash(b"curs3d");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_merkle_root() {
        let hashes = vec![sha3_hash(b"tx1"), sha3_hash(b"tx2"), sha3_hash(b"tx3")];
        let root = merkle_root(&hashes);
        assert_eq!(root.len(), 32);
    }

    #[test]
    fn test_merkle_proof_roundtrip() {
        let hashes = vec![
            sha3_hash(b"chunk1"),
            sha3_hash(b"chunk2"),
            sha3_hash(b"chunk3"),
            sha3_hash(b"chunk4"),
        ];
        let root = merkle_root(&hashes);
        let proof = merkle_proof(&hashes, 2);
        assert!(verify_merkle_proof(&hashes[2], &proof, 2, &root));
    }

    #[test]
    fn test_address_derivation() {
        let public_key = vec![7; 32];
        let address = address_bytes_from_public_key(&public_key);
        assert_eq!(address.len(), ADDRESS_LEN);
        assert!(address_string_from_public_key(&public_key).starts_with("CUR"));
    }

    #[test]
    fn test_domain_separation_produces_different_hashes() {
        let data = b"same data";
        let h1 = sha3_hash_domain(b"domain-a", &[data]);
        let h2 = sha3_hash_domain(b"domain-b", &[data]);
        let h3 = sha3_hash(data);
        assert_ne!(h1, h2);
        assert_ne!(h1, h3);
        assert_ne!(h2, h3);
    }

    #[test]
    fn test_checksum_address_roundtrip() {
        let addr_bytes = vec![
            0xde, 0xad, 0xbe, 0xef, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a,
            0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
        ];
        let checksummed = checksum_address(&addr_bytes);
        assert!(checksummed.starts_with("CUR"));
        assert_eq!(checksummed.len(), 3 + 40);
        assert!(verify_checksum_address(&checksummed));
    }

    #[test]
    fn test_checksum_address_rejects_bad_checksum() {
        let addr_bytes = vec![0xab; 20];
        let mut checksummed = checksum_address(&addr_bytes);
        // Flip a character case to break checksum
        let hex_part = checksummed.strip_prefix("CUR").unwrap().to_string();
        let mut chars: Vec<char> = hex_part.chars().collect();
        for c in &mut chars {
            if c.is_ascii_alphabetic() {
                if c.is_ascii_uppercase() {
                    *c = c.to_ascii_lowercase();
                } else {
                    *c = c.to_ascii_uppercase();
                }
                break;
            }
        }
        let broken: String = chars.into_iter().collect();
        checksummed = format!("CUR{}", broken);
        assert!(!verify_checksum_address(&checksummed));
    }
}
