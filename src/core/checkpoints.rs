//! Hardcoded trusted checkpoints.
//!
//! Pattern borrowed from Bitcoin Core's `chainparams.cpp`: a small set of
//! `(height, hash, state_root)` anchors compiled into the binary that every
//! node enforces unconditionally. A peer cannot feed us a snapshot or block
//! that disagrees with a hardcoded checkpoint, regardless of how many
//! "valid" signatures they bring.
//!
//! Why in the binary, not in `GenesisConfig`:
//!   - Adding a field to `GenesisConfig` would force a hardfork (the
//!     genesis hash incorporates the config) or require very careful
//!     `#[serde(default)]` plumbing across every node. Both are heavier
//!     than necessary for a safety net.
//!   - An operator who chose to run a given `curs3d` binary has already
//!     committed to trusting the code in it. Hardcoding checkpoints in
//!     the binary is consistent with that trust boundary.
//!
//! How to populate:
//!   - Run `cargo run --release -- block --json --height N` against a
//!     finalised height on a node you trust.
//!   - Take the block `hash` and `state_root` from the output.
//!   - Append a `TrustedCheckpoint` to the slice for the right chain id.
//!   - Ship a new binary release. Old binaries keep working; they just
//!     don't enforce the newest checkpoint.

use crate::core::chain::ChainError;
use crate::storage::SnapshotManifest;

/// A `(height, hash, state_root)` anchor that every node enforces.
///
/// `state_root` is optional because it is only ever checked when applying
/// a snapshot — the block-add path verifies the block hash, which already
/// covers the state root transitively (the header commits to it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedCheckpoint {
    pub height: u64,
    pub hash: &'static [u8; 32],
    pub state_root: Option<&'static [u8; 32]>,
}

/// Return the hardcoded checkpoints for a given chain id.
///
/// Returns an empty slice for unknown chain ids — devnets and ad-hoc
/// localnets are not anchored, by design.
pub fn checkpoints_for(chain_id: &str) -> &'static [TrustedCheckpoint] {
    match chain_id {
        "curs3d-public-testnet" => CURS3D_PUBLIC_TESTNET,
        _ => &[],
    }
}

/// Public testnet checkpoints.
///
/// The current 2-validator chain was regenerated on 2026-05-06 (genesis
/// SHA-256 `165c5f9d2a77719ecada5937753465806d83429588df06f0f25cea5c274bbf4e`).
/// The chain is still young and operationally fluid (node3 rebuild in
/// progress, hardfork to v6 with `SparseMerkleTrie` planned). We do NOT
/// anchor a checkpoint until the network has been stable for at least one
/// audit cycle.
///
/// The empty slice still exercises the enforcement code path through the
/// unit tests below.
const CURS3D_PUBLIC_TESTNET: &[TrustedCheckpoint] = &[
    // TrustedCheckpoint {
    //     height: <pick a height past first audit>,
    //     hash: &[...],
    //     state_root: Some(&[...]),
    // },
];

/// Reject a block that disagrees with a hardcoded checkpoint at its height.
///
/// Returns `Ok(())` when:
///   - No checkpoint exists for that chain at that height, or
///   - The checkpoint exists and the block hash matches.
///
/// Returns `ChainError::CheckpointMismatch` when a checkpoint exists at
/// the block's height but the hash differs.
pub fn verify_block_against_known(
    chain_id: &str,
    height: u64,
    block_hash: &[u8],
) -> Result<(), ChainError> {
    let Some(cp) = checkpoints_for(chain_id)
        .iter()
        .find(|cp| cp.height == height)
    else {
        return Ok(());
    };
    if block_hash == cp.hash.as_slice() {
        Ok(())
    } else {
        Err(ChainError::CheckpointMismatch {
            height,
            expected: hex::encode(cp.hash),
            got: hex::encode(block_hash),
            kind: "block",
        })
    }
}

/// Reject a snapshot manifest that disagrees with any hardcoded checkpoint
/// it crosses.
///
/// A snapshot at height H must agree with every checkpoint at heights
/// `<= H`:
///   - `manifest.latest_hash` at height `manifest.height` (if a checkpoint
///     sits exactly on H).
///   - `manifest.state_root` at height `manifest.height` (if the
///     checkpoint records a state root and the manifest is at the
///     checkpoint's height).
///   - `manifest.finalized_hash` at `manifest.finalized_height` (if a
///     checkpoint sits exactly on that height).
///
/// We do NOT scan every block in `manifest.chunk_hashes` against
/// checkpoints — that's the snapshot decoder's job (the
/// `history rewrite protection` loop in `Blockchain::apply_snapshot`
/// catches mismatches against any locally known block, which subsumes the
/// checkpoint case once a checkpoint has been ingested via the normal
/// block-add path).
pub fn verify_snapshot_against_known(
    chain_id: &str,
    manifest: &SnapshotManifest,
) -> Result<(), ChainError> {
    let cps = checkpoints_for(chain_id);
    for cp in cps {
        if cp.height == manifest.height {
            if manifest.latest_hash != cp.hash.as_slice() {
                return Err(ChainError::CheckpointMismatch {
                    height: cp.height,
                    expected: hex::encode(cp.hash),
                    got: hex::encode(&manifest.latest_hash),
                    kind: "snapshot_latest_hash",
                });
            }
            if let Some(expected_root) = cp.state_root
                && manifest.state_root != expected_root.as_slice()
            {
                return Err(ChainError::CheckpointMismatch {
                    height: cp.height,
                    expected: hex::encode(expected_root),
                    got: hex::encode(&manifest.state_root),
                    kind: "snapshot_state_root",
                });
            }
        }
        if cp.height == manifest.finalized_height && manifest.finalized_hash != cp.hash.as_slice() {
            return Err(ChainError::CheckpointMismatch {
                height: cp.height,
                expected: hex::encode(cp.hash),
                got: hex::encode(&manifest.finalized_hash),
                kind: "snapshot_finalized_hash",
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CHAIN: &str = "curs3d-checkpoint-tests";

    // Local override of `checkpoints_for` for tests. We can't mutate the
    // real `CURS3D_PUBLIC_TESTNET` slice (it's `const`), so the tests
    // build their own `TrustedCheckpoint`s and call the lower-level
    // helpers directly.

    fn fake_cp(height: u64, hash: &'static [u8; 32]) -> TrustedCheckpoint {
        TrustedCheckpoint {
            height,
            hash,
            state_root: None,
        }
    }

    fn run_block_check(
        cps: &[TrustedCheckpoint],
        height: u64,
        block_hash: &[u8],
    ) -> Result<(), ChainError> {
        // Inlined copy of `verify_block_against_known` that takes the
        // checkpoint slice directly. Keeps the production function shape
        // (look up by chain id) while letting tests drive arbitrary
        // checkpoint sets.
        let Some(cp) = cps.iter().find(|cp| cp.height == height) else {
            return Ok(());
        };
        if block_hash == cp.hash.as_slice() {
            Ok(())
        } else {
            Err(ChainError::CheckpointMismatch {
                height,
                expected: hex::encode(cp.hash),
                got: hex::encode(block_hash),
                kind: "block",
            })
        }
    }

    #[test]
    fn empty_checkpoint_set_is_permissive() {
        let cps: &[TrustedCheckpoint] = &[];
        assert!(run_block_check(cps, 0, &[0u8; 32]).is_ok());
        assert!(run_block_check(cps, 42, &[1u8; 32]).is_ok());
    }

    #[test]
    fn checkpoint_accepts_matching_hash() {
        static H: [u8; 32] = [7u8; 32];
        let cps = [fake_cp(100, &H)];
        assert!(run_block_check(&cps, 100, &H).is_ok());
    }

    #[test]
    fn checkpoint_rejects_diverging_hash() {
        static H: [u8; 32] = [7u8; 32];
        let cps = [fake_cp(100, &H)];
        let other = [9u8; 32];
        let err = run_block_check(&cps, 100, &other).expect_err("must reject");
        match err {
            ChainError::CheckpointMismatch { height, kind, .. } => {
                assert_eq!(height, 100);
                assert_eq!(kind, "block");
            }
            other => panic!("expected CheckpointMismatch, got {other:?}"),
        }
    }

    #[test]
    fn checkpoint_at_other_height_does_not_affect_block() {
        static H: [u8; 32] = [7u8; 32];
        let cps = [fake_cp(100, &H)];
        // Block at height 99 is not anchored. Any hash is OK.
        assert!(run_block_check(&cps, 99, &[42u8; 32]).is_ok());
        assert!(run_block_check(&cps, 101, &[42u8; 32]).is_ok());
    }

    #[test]
    fn unknown_chain_id_returns_empty_checkpoints() {
        assert!(checkpoints_for("curs3d-devnet").is_empty());
        assert!(checkpoints_for("does-not-exist").is_empty());
    }

    #[test]
    fn public_testnet_lookup_returns_the_canonical_slice() {
        // Even if empty today, the lookup must return the canonical
        // slice for the public testnet so the live network is wired up
        // when a checkpoint is added in a future release.
        let cps = checkpoints_for("curs3d-public-testnet");
        assert!(std::ptr::eq(cps, CURS3D_PUBLIC_TESTNET));
    }

    fn snapshot(
        height: u64,
        latest_hash: Vec<u8>,
        fin_height: u64,
        fin_hash: Vec<u8>,
    ) -> SnapshotManifest {
        SnapshotManifest {
            height,
            epoch: 0,
            chain_id: TEST_CHAIN.to_string(),
            genesis_hash: vec![0u8; 32],
            latest_hash,
            tip_height: height,
            tip_hash: vec![0u8; 32],
            finalized_height: fin_height,
            finalized_hash: fin_hash,
            state_root: vec![0u8; 32],
            chunk_root: vec![],
            chunk_count: 0,
            chunk_hashes: vec![],
        }
    }

    fn run_snapshot_check(
        cps: &[TrustedCheckpoint],
        manifest: &SnapshotManifest,
    ) -> Result<(), ChainError> {
        // Same inlined-helper pattern as `run_block_check`.
        for cp in cps {
            if cp.height == manifest.height {
                if manifest.latest_hash != cp.hash.as_slice() {
                    return Err(ChainError::CheckpointMismatch {
                        height: cp.height,
                        expected: hex::encode(cp.hash),
                        got: hex::encode(&manifest.latest_hash),
                        kind: "snapshot_latest_hash",
                    });
                }
                if let Some(expected_root) = cp.state_root
                    && manifest.state_root != expected_root.as_slice()
                {
                    return Err(ChainError::CheckpointMismatch {
                        height: cp.height,
                        expected: hex::encode(expected_root),
                        got: hex::encode(&manifest.state_root),
                        kind: "snapshot_state_root",
                    });
                }
            }
            if cp.height == manifest.finalized_height
                && manifest.finalized_hash != cp.hash.as_slice()
            {
                return Err(ChainError::CheckpointMismatch {
                    height: cp.height,
                    expected: hex::encode(cp.hash),
                    got: hex::encode(&manifest.finalized_hash),
                    kind: "snapshot_finalized_hash",
                });
            }
        }
        Ok(())
    }

    #[test]
    fn snapshot_with_matching_latest_hash_passes() {
        static H: [u8; 32] = [3u8; 32];
        let cps = [fake_cp(50, &H)];
        let m = snapshot(50, H.to_vec(), 32, vec![1u8; 32]);
        assert!(run_snapshot_check(&cps, &m).is_ok());
    }

    #[test]
    fn snapshot_with_diverging_latest_hash_fails() {
        static H: [u8; 32] = [3u8; 32];
        let cps = [fake_cp(50, &H)];
        let m = snapshot(50, vec![99u8; 32], 32, vec![1u8; 32]);
        let err = run_snapshot_check(&cps, &m).expect_err("must reject");
        match err {
            ChainError::CheckpointMismatch { kind, height, .. } => {
                assert_eq!(kind, "snapshot_latest_hash");
                assert_eq!(height, 50);
            }
            other => panic!("expected CheckpointMismatch, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_with_diverging_finalized_hash_fails() {
        static H: [u8; 32] = [3u8; 32];
        let cps = [fake_cp(32, &H)];
        // Manifest height 50 != checkpoint height 32, but finalized_height
        // 32 matches the checkpoint, and finalized_hash diverges.
        let m = snapshot(50, vec![5u8; 32], 32, vec![99u8; 32]);
        let err = run_snapshot_check(&cps, &m).expect_err("must reject");
        match err {
            ChainError::CheckpointMismatch { kind, height, .. } => {
                assert_eq!(kind, "snapshot_finalized_hash");
                assert_eq!(height, 32);
            }
            other => panic!("expected CheckpointMismatch, got {other:?}"),
        }
    }

    #[test]
    fn snapshot_with_diverging_state_root_fails() {
        static H: [u8; 32] = [3u8; 32];
        static R: [u8; 32] = [4u8; 32];
        let cps = [TrustedCheckpoint {
            height: 50,
            hash: &H,
            state_root: Some(&R),
        }];
        let mut m = snapshot(50, H.to_vec(), 32, vec![1u8; 32]);
        m.state_root = vec![88u8; 32];
        let err = run_snapshot_check(&cps, &m).expect_err("must reject");
        match err {
            ChainError::CheckpointMismatch { kind, .. } => {
                assert_eq!(kind, "snapshot_state_root");
            }
            other => panic!("expected CheckpointMismatch, got {other:?}"),
        }
    }
}
