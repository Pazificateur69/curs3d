#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 36 {
        return;
    }

    // Use first 32 bytes as a leaf, next 4 bytes as index
    let leaf = data[..32].to_vec();
    let index = u32::from_le_bytes([data[32], data[33], data[34], data[35]]) as usize;

    // Build variable-length proof from remaining data (chunks of 32 bytes)
    let remaining = &data[36..];
    let proof: Vec<Vec<u8>> = remaining
        .chunks(32)
        .filter(|chunk| chunk.len() == 32)
        .map(|chunk| chunk.to_vec())
        .collect();

    if proof.is_empty() {
        return;
    }

    // Create a fake root from first proof element
    let root = curs3d::crypto::hash::sha3_hash(&proof[0]);

    // This should never panic
    let _ = curs3d::crypto::hash::verify_merkle_proof(&leaf, index, &proof, &root);
});
