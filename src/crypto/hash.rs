use sha3::{Digest, Sha3_256};

pub const ADDRESS_LEN: usize = 20;

pub fn sha3_hash(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha3_256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

pub fn double_hash(data: &[u8]) -> Vec<u8> {
    sha3_hash(&sha3_hash(data))
}

pub fn address_bytes_from_public_key(public_key: &[u8]) -> Vec<u8> {
    let hash = sha3_hash(public_key);
    hash[..ADDRESS_LEN].to_vec()
}

pub fn address_string_from_public_key(public_key: &[u8]) -> String {
    format!(
        "CUR{}",
        hex::encode(address_bytes_from_public_key(public_key))
    )
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
    fn test_address_derivation() {
        let public_key = vec![7; 32];
        let address = address_bytes_from_public_key(&public_key);
        assert_eq!(address.len(), ADDRESS_LEN);
        assert!(address_string_from_public_key(&public_key).starts_with("CUR"));
    }
}
