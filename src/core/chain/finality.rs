//! Finality votes, equivocation slashing, and the prune-on-finalize
//! hook for #28 Phase E.
//!
//! Child module of `chain`, so private fields of `Blockchain`
//! (`cursor`, `prune_keep_blocks`, etc.) and private helpers
//! (`persist_finalized_height`, `persist_equivocation`) are visible
//! without elevating visibility.
//!
//! Extracted from `chain/mod.rs` in #29:
//! - [`Blockchain::add_finality_vote`] — record a vote, advance finality,
//!   fire the prune hook.
//! - [`Blockchain::process_equivocation`] — slash a validator on
//!   double-sign evidence.
//! - [`Blockchain::finalized_height`] — getter for /api/metrics + UI.
//! - [`Blockchain::maybe_prune_finalized`] — drop blocks below
//!   `finalized - keep_blocks` (#28 Phase E).

use super::{Blockchain, ChainError};
use crate::consensus::{EquivocationEvidence, FinalityVote, FinalizedBlock, ProofOfStake};
use crate::crypto::hash;

impl Blockchain {
    pub fn add_finality_vote(&mut self, vote: FinalityVote) -> Option<FinalizedBlock> {
        let voted_block = self.block_tree.get(&vote.block_hash)?;
        if voted_block.header.height != vote.block_height {
            return None;
        }
        let vote_epoch = self.epoch_for_height(vote.block_height);
        if vote.epoch != vote_epoch {
            return None;
        }
        if !self.block_tree.is_on_canonical_chain(&vote.block_hash) {
            return None;
        }
        let snapshot = self.epoch_snapshots.get(&vote_epoch)?;

        let result = self.finality_tracker.add_vote(vote, snapshot);

        if let Some(ref finalized) = result {
            self.block_tree
                .set_finalized(finalized.hash.clone(), finalized.height);

            self.persist_finalized_height(finalized.height);

            tracing::info!(
                "Block #{} finalized (hash: {})",
                finalized.height,
                hex::encode(&finalized.hash[..8])
            );
            tracing::info!(
                target: "audit",
                event = "block_finalized",
                height = finalized.height,
                hash = %hex::encode(&finalized.hash),
            );

            // #28 Phase E — drop history below the prune watermark every
            // finalization. No-op in archival mode. Safe because anything
            // strictly below the finalized height can never be needed for
            // a future reorg (the chain's own ReorgBelowFinality guard
            // enforces that invariant).
            let _removed = self.maybe_prune_finalized(finalized.height);
        }

        result
    }

    /// Process equivocation evidence: slash the offending validator
    /// (33% of staked balance + jail for `jail_duration_blocks`).
    /// Returns the amount slashed in microtokens.
    pub fn process_equivocation(
        &mut self,
        evidence: &EquivocationEvidence,
    ) -> Result<u64, crate::consensus::SlashingError> {
        let mut pos = ProofOfStake::with_slashed(
            self.minimum_stake,
            self.slashed_validators.clone(),
            self.height(),
        );
        let penalty =
            pos.slash_with_evidence(&mut self.accounts, evidence, self.jail_duration_blocks)?;
        self.slashed_validators = pos.slashed_validators;

        self.persist_equivocation(evidence);

        tracing::warn!(
            target: "audit",
            event = "validator_slashed",
            validator = %hex::encode(hash::address_bytes_from_public_key(&evidence.validator_public_key)),
            height = evidence.height,
            penalty = penalty,
        );

        Ok(penalty)
    }

    pub fn finalized_height(&self) -> u64 {
        self.finality_tracker.finalized_height
    }

    /// Apply the prune policy at a finalization event. If pruning is
    /// disabled this is a no-op. Otherwise drops every block below
    /// `finalized_height - keep`. Logged at `audit` info level on
    /// non-trivial prunes so the operator can see retention in journald.
    /// Pure side effect — returns the number of blocks actually
    /// removed for tests / metrics.
    pub(super) fn maybe_prune_finalized(&mut self, finalized_height: u64) -> usize {
        let Some(keep) = self.prune_keep_blocks else {
            return 0;
        };
        // Need at least `keep + 1` blocks below the finalized height for a
        // prune to make sense (we keep heights [keep_from, finalized]).
        let Some(keep_from) = finalized_height.checked_sub(keep) else {
            return 0;
        };
        if keep_from == 0 {
            // Genesis pin already protects 0; anything below is empty.
            return 0;
        }
        match self.cursor.prune_below(keep_from) {
            Ok(removed) => {
                if removed > 0 {
                    tracing::info!(
                        target: "audit",
                        event = "blocks_pruned",
                        keep_from,
                        finalized = finalized_height,
                        removed,
                    );
                }
                removed
            }
            Err(e) => {
                tracing::warn!(error = %e, "cursor.prune_below failed at finality");
                0
            }
        }
    }
}

// Re-export to silence the unused-import warning in `mod.rs` —
// `ChainError` is used internally for parity with the original block
// even though no public-facing method here returns it; keeping the
// import groups consistent across submodules makes future moves
// mechanical.
#[allow(dead_code)]
type _ChainErrorAlias = ChainError;
