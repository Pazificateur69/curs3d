#![no_main]
//! Fuzz target for `verify_merkle_proof` and `merkle_root`.
//!
//! Catches panics + crashes from malformed proofs / leaves / indices.
//! Also exercises `merkle_root` (the dual-prefix Merkle implementation
//! that fixed the 2nd-preimage attack — fuzzing here helps catch any
//! regression that would re-introduce it).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 36 {
        return;
    }

    // First 32 bytes = leaf hash. Next 4 bytes = index.
    let leaf = data[..32].to_vec();
    let index = u32::from_le_bytes([data[32], data[33], data[34], data[35]]) as usize;

    // Build variable-length proof from remaining data (chunks of 32 bytes).
    let remaining = &data[36..];
    let proof: Vec<Vec<u8>> = remaining
        .chunks(32)
        .filter(|chunk| chunk.len() == 32)
        .map(|chunk| chunk.to_vec())
        .collect();

    // Fake root derived from the leaf — most calls will return false,
    // we're checking the function never panics on adversarial input.
    let root = curs3d::crypto::hash::sha3_hash(&leaf);

    // Signature: verify_merkle_proof(leaf, proof, index, root).
    let _ = curs3d::crypto::hash::verify_merkle_proof(&leaf, &proof, index, &root);

    // Also exercise merkle_root on the same data — the prefix fix
    // (0x00 leaf / 0x01 node) must never panic, including for empty
    // inputs or single-leaf cases.
    let _ = curs3d::crypto::hash::merkle_root(&proof);

    // And verify the round-trip property on small inputs: a randomly
    // selected index from a non-empty leaf set must produce a proof
    // that verifies against the computed root.
    if !proof.is_empty() {
        let idx = (index % proof.len()).min(proof.len() - 1);
        let computed_root = curs3d::crypto::hash::merkle_root(&proof);
        let computed_proof = curs3d::crypto::hash::merkle_proof(&proof, idx);
        let verified = curs3d::crypto::hash::verify_merkle_proof(
            &proof[idx],
            &computed_proof,
            idx,
            &computed_root,
        );
        // Round-trip MUST verify — catches Merkle implementation regressions.
        assert!(verified, "merkle round-trip failed: idx={idx}, leaves={}", proof.len());
    }
});
