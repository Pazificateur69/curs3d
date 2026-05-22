//! Mempool admission, sorting, pruning, and limit enforcement.
//!
//! Lifted out of `chain/mod.rs` in #29. Child module of `chain`, so
//! private fields of `Blockchain` (`storage`, `persistence`, etc.) and
//! private constants (`MAX_PENDING_GAS_BUDGET_MULTIPLIER`,
//! `MAX_PENDING_TRANSACTIONS_USER`, `RESERVED_SYSTEM_SLOTS`,
//! `MAX_PENDING_TX_AGE_SECS`) are visible without elevating visibility.
//!
//! Currently extracted:
//! - mempool stats / classification helpers
//! - eviction (`worst_pending_transaction_index_in_class`,
//!   `evict_transaction_and_dependents`, `enforce_mempool_limits`)
//! - sort + prune for the pending pool
//! - fee floors (`minimum_admission_fee`,
//!   `minimum_priority_fee_per_gas`, `*_for_usage`,
//!   `compare_fee_priority`)
//!
//! `add_transaction` itself stays in `mod.rs` — it's a 200-line orchestration
//! method that calls into many other Blockchain pieces, and moving it
//! belongs in a follow-up extraction.

use super::{
    Blockchain, ChainError, MAX_PENDING_GAS_BUDGET_MULTIPLIER, MAX_PENDING_TRANSACTIONS_USER,
    MAX_PENDING_TX_AGE_SECS, RESERVED_SYSTEM_SLOTS,
};
use crate::core::transaction::{MempoolClass, Transaction};

impl Blockchain {
    /// Aggregated mempool stats for /api/metrics. Returns
    /// `(system_count, user_count, gas_usage, gas_budget)`. Cheap O(n)
    /// on pending_transactions; pending is bounded by
    /// `MAX_PENDING_TRANSACTIONS` so this is fine for the hot poll path.
    pub fn mempool_stats(&self) -> (usize, usize, u64, u64) {
        let (system_count, user_count) = self.count_pending_by_class();
        let gas_usage = self.pending_gas_usage();
        let gas_budget = self.pending_gas_budget();
        (system_count, user_count, gas_usage, gas_budget)
    }

    pub(super) fn sort_pending_transactions(&mut self) {
        let base_fee_per_gas = self.next_base_fee_per_gas(&self.latest_block());
        self.pending_transactions.sort_by(|a, b| {
            if a.from == b.from {
                // Same sender: nonce order is mandatory regardless of
                // class — a Stake at nonce N+1 cannot be applied before
                // the Transfer at nonce N. Class-based reordering would
                // break nonce sequencing.
                a.nonce
                    .cmp(&b.nonce)
                    .then_with(|| b.max_fee_per_gas().cmp(&a.max_fee_per_gas()))
            } else {
                // Different senders: System class sorts strictly ahead
                // of User class. Block production includes from the
                // front, so system txs land in blocks first when the
                // block has capacity. Within a class, the existing
                // fee-priority + timestamp tiebreak applies.
                let class_order = match (a.mempool_class(), b.mempool_class()) {
                    (MempoolClass::System, MempoolClass::User) => {
                        return std::cmp::Ordering::Less;
                    }
                    (MempoolClass::User, MempoolClass::System) => {
                        return std::cmp::Ordering::Greater;
                    }
                    _ => std::cmp::Ordering::Equal,
                };
                class_order
                    .then_with(|| Self::compare_fee_priority(a, b, base_fee_per_gas))
                    .then_with(|| a.timestamp.cmp(&b.timestamp))
                    .then_with(|| b.max_fee_per_gas().cmp(&a.max_fee_per_gas()))
            }
        });
    }

    pub(super) fn prune_pending_transactions(&mut self) {
        let cutoff = chrono::Utc::now().timestamp() - MAX_PENDING_TX_AGE_SECS;
        let base_fee_per_gas = self.next_base_fee_per_gas(&self.latest_block());
        let usage = self.pending_gas_usage();
        let budget = self.pending_gas_budget().max(1);
        self.pending_transactions.retain(|pending| {
            pending.timestamp >= cutoff
                && pending.max_fee_per_gas() >= base_fee_per_gas
                && pending
                    .priority_fee_per_gas(base_fee_per_gas)
                    .unwrap_or_default()
                    >= Self::minimum_priority_fee_per_gas_for_usage(pending, usage, budget)
        });
        self.sort_pending_transactions();
    }

    pub(super) fn compare_fee_priority(
        a: &Transaction,
        b: &Transaction,
        base_fee_per_gas: u64,
    ) -> std::cmp::Ordering {
        let a_gas = a.estimated_gas_for_admission().max(1) as u128;
        let b_gas = b.estimated_gas_for_admission().max(1) as u128;
        let a_fee = a.priority_fee_per_gas(base_fee_per_gas).unwrap_or_default() as u128;
        let b_fee = b.priority_fee_per_gas(base_fee_per_gas).unwrap_or_default() as u128;
        (b_fee.saturating_mul(a_gas))
            .cmp(&a_fee.saturating_mul(b_gas))
            .then_with(|| b.max_fee_per_gas().cmp(&a.max_fee_per_gas()))
    }

    pub(super) fn pending_gas_budget(&self) -> u64 {
        self.block_gas_limit
            .saturating_mul(MAX_PENDING_GAS_BUDGET_MULTIPLIER)
    }

    pub(super) fn pending_gas_usage(&self) -> u64 {
        self.pending_transactions
            .iter()
            .map(Transaction::estimated_gas_for_admission)
            .sum()
    }

    pub(super) fn minimum_admission_fee(&self, tx: &Transaction, base_fee_per_gas: u64) -> u64 {
        let required_base = tx.effective_gas_limit().saturating_mul(base_fee_per_gas);
        let required_priority = tx
            .effective_gas_limit()
            .saturating_mul(self.minimum_priority_fee_per_gas(tx, base_fee_per_gas));
        let usage = self.pending_gas_usage();
        let budget = self.pending_gas_budget().max(1);
        let occupancy_pct = usage.saturating_mul(100) / budget;
        let surcharge = if occupancy_pct >= 95 {
            8
        } else if occupancy_pct >= 85 {
            4
        } else if occupancy_pct >= 70 {
            2
        } else if occupancy_pct >= 50 {
            1
        } else {
            0
        };
        if surcharge == 0 {
            return required_base.saturating_add(required_priority);
        }
        let units = tx.estimated_gas_for_admission().saturating_add(99_999) / 100_000;
        required_base
            .saturating_add(required_priority)
            .saturating_add(units.max(1).saturating_mul(surcharge))
    }

    pub(super) fn minimum_priority_fee_per_gas(
        &self,
        tx: &Transaction,
        _base_fee_per_gas: u64,
    ) -> u64 {
        let usage = self.pending_gas_usage();
        let budget = self.pending_gas_budget().max(1);
        Self::minimum_priority_fee_per_gas_for_usage(tx, usage, budget)
    }

    pub(super) fn minimum_priority_fee_per_gas_for_usage(
        tx: &Transaction,
        usage: u64,
        budget: u64,
    ) -> u64 {
        let occupancy_pct = usage.saturating_mul(100) / budget;
        let congestion_floor: u64 = if occupancy_pct >= 95 {
            12
        } else if occupancy_pct >= 85 {
            8
        } else if occupancy_pct >= 70 {
            4
        } else if occupancy_pct >= 50 {
            2
        } else {
            0
        };
        let gas_units = tx.estimated_gas_for_admission().saturating_add(249_999) / 250_000;
        congestion_floor.saturating_mul(gas_units.max(1))
    }

    /// Lowest-fee eviction candidate restricted to a class. Used by
    /// `enforce_mempool_limits` so user pressure never evicts a system
    /// transaction. With no class filtering, the original
    /// `worst_pending_transaction_index` was the same algorithm with
    /// `filter = |_| true`; that path is no longer reachable because
    /// every caller is class-aware now.
    pub(super) fn worst_pending_transaction_index_in_class(
        &self,
        class: MempoolClass,
    ) -> Option<usize> {
        let base_fee_per_gas = self.next_base_fee_per_gas(&self.latest_block());
        self.pending_transactions
            .iter()
            .enumerate()
            .filter(|(_, tx)| tx.mempool_class() == class)
            .min_by(|(_, a), (_, b)| {
                if a.from == b.from {
                    b.nonce
                        .cmp(&a.nonce)
                        .then_with(|| a.max_fee_per_gas().cmp(&b.max_fee_per_gas()))
                } else {
                    Self::compare_fee_priority(a, b, base_fee_per_gas).reverse()
                }
            })
            .map(|(index, _)| index)
    }

    pub(super) fn count_pending_by_class(&self) -> (usize, usize) {
        let mut system = 0usize;
        let mut user = 0usize;
        for tx in &self.pending_transactions {
            match tx.mempool_class() {
                MempoolClass::System => system += 1,
                MempoolClass::User => user += 1,
            }
        }
        (system, user)
    }

    pub(super) fn evict_transaction_and_dependents(&mut self, index: usize) {
        if index >= self.pending_transactions.len() {
            return;
        }
        let evicted = self.pending_transactions.remove(index);
        self.pending_transactions
            .retain(|pending| !(pending.from == evicted.from && pending.nonce > evicted.nonce));
    }

    pub(super) fn enforce_mempool_limits(
        &mut self,
        protected_hash: &[u8],
    ) -> Result<(), ChainError> {
        loop {
            let (system_count, user_count) = self.count_pending_by_class();
            let over_user_count = user_count > MAX_PENDING_TRANSACTIONS_USER;
            let over_system_count = system_count > RESERVED_SYSTEM_SLOTS;
            let over_gas = self.pending_gas_usage() > self.pending_gas_budget();
            if !over_user_count && !over_system_count && !over_gas {
                break;
            }

            // Eviction policy: System class is fully protected from user
            // pressure. Always prefer to evict the worst User-class
            // transaction first, regardless of which bound was exceeded.
            // The only path that touches System is when the User pool is
            // empty and we're still over a bound — that means the System
            // pool itself is the source of the overage (a
            // pathological stake/governance flood is its own protocol
            // bug, but we still evict its worst entry rather than
            // leaving the node wedged).
            let index = self
                .worst_pending_transaction_index_in_class(MempoolClass::User)
                .or_else(|| self.worst_pending_transaction_index_in_class(MempoolClass::System));
            let Some(index) = index else { break };
            let is_protected = self.pending_transactions[index].hash() == protected_hash;
            if is_protected {
                if over_gas {
                    return Err(ChainError::FeeTooLow);
                }
                return Err(ChainError::MempoolFull);
            }
            self.evict_transaction_and_dependents(index);
        }
        Ok(())
    }
}
