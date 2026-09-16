use crate::mempool::dependencies::DependencyTracker;
use crate::mempool::storage::TransactionStorage;
use crate::transactions::{Transaction, TransactionHash};
use std::collections::HashSet;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RbfConflictError {
    #[error(
        "New transaction fee is too low to replace the existing ones. Required: {required}, got: {actual}"
    )]
    InsufficientFee { required: u64, actual: u64 },

    #[error("Conflict resolution failed: {0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct RbfPolicy {
    /// Minimum absolute fee
    pub base_fee_delta: u64,
    /// Minimum percentage fee increase (e.g., 0.10 for 10%).
    pub percentage_increase: f64,
}

impl Default for RbfPolicy {
    fn default() -> Self {
        RbfPolicy::new(1000, 0.10)
    }
}

impl RbfPolicy {
    pub fn new(base_fee_delta: u64, percentage_increase: f64) -> Self {
        Self {
            base_fee_delta,
            percentage_increase,
        }
    }

    /// Creates a disabled RBF policy.
    pub fn disabled() -> Self {
        Self {
            base_fee_delta: 0,
            percentage_increase: 0.0,
        }
    }
    /// Checks if the RBF policy is disabled (i.e., no fee increase required).
    pub fn is_disabled(&self) -> bool {
        self.base_fee_delta == 0 && self.percentage_increase == 0.0
    }

    /// Computes the required fee to replace an existing transaction fee under a given load factor.
    pub fn required_fee(&self, old_fee: u64, load_factor: f64) -> u64 {
        let required_absolute = old_fee.saturating_add(self.base_fee_delta);
        let required_percentage =
            ((old_fee as f64) * (1.0 + self.percentage_increase)).ceil() as u64;
        let base_required_fee = std::cmp::max(required_absolute, required_percentage);
        ((base_required_fee as f64) * load_factor).ceil() as u64
    }

    pub fn can_replace(&self, new_fee: u64, old_fee: u64, load_factor: f64) -> bool {
        new_fee >= self.required_fee(old_fee, load_factor)
    }
}

pub struct RbfConflictResolver {
    policy: RbfPolicy,
}

impl RbfConflictResolver {
    pub fn new(policy: RbfPolicy) -> Self {
        Self { policy }
    }

    pub fn find_conflicts(
        &self,
        new_tx: &Transaction,
        storage: &TransactionStorage,
    ) -> HashSet<TransactionHash> {
        let mut conflicts = HashSet::new();

        for inp in &new_tx.data.inputs {
            if let Some(existing_hash) = storage.get_spending_tx(&inp.previous_output) {
                conflicts.insert(*existing_hash);
            }
        }

        conflicts
    }

    pub fn resolve_conflicts(
        &self,
        conflicts: &HashSet<TransactionHash>,
        storage: &mut TransactionStorage,
        tracker: &mut DependencyTracker,
        new_fee: u64,
        load_factor: f64,
    ) -> Result<(), RbfConflictError> {
        if conflicts.is_empty() {
            return Ok(());
        }

        let mut total_conflict_fee = 0u64;
        let mut all_affected_txs = HashSet::new();

        for conflict_hash in conflicts {
            if let Some(conflict_tx) = storage.get(conflict_hash) {
                total_conflict_fee = total_conflict_fee.saturating_add(conflict_tx.fee);
                all_affected_txs.insert(*conflict_hash);

                let descendants = tracker.get_descendants(conflict_hash);
                for desc_hash in &descendants {
                    all_affected_txs.insert(*desc_hash);

                    if let Some(desc_tx) = storage.get(desc_hash) {
                        total_conflict_fee = total_conflict_fee.saturating_add(desc_tx.fee);
                    }
                }
            }
        }

        let conflict_count_factor = 1.0 + (conflicts.len() as f64 * 0.05);
        let base_required_fee = self.policy.required_fee(total_conflict_fee, load_factor);
        let required_fee = (base_required_fee as f64 * conflict_count_factor).ceil() as u64;

        if new_fee < required_fee {
            return Err(RbfConflictError::InsufficientFee {
                required: required_fee,
                actual: new_fee,
            });
        }

        let mut affected_by_depth: Vec<&TransactionHash> = all_affected_txs.iter().collect();
        affected_by_depth.sort_by(|a, b| {
            let a_deps = tracker.get_descendants(a).len();
            let b_deps = tracker.get_descendants(b).len();
            b_deps.cmp(&a_deps)
        });

        for tx_hash in &affected_by_depth {
            storage.remove(tx_hash);
            tracker.remove_transaction(tx_hash);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_required_fee_normal_load() {
        let policy = RbfPolicy::new(1000, 0.10);
        let old_fee = 5000;
        assert_eq!(policy.required_fee(old_fee, 1.0), 6000);
    }

    #[test]
    fn test_required_fee_high_load() {
        let policy = RbfPolicy::new(1000, 0.10);
        let old_fee = 5000;
        assert_eq!(policy.required_fee(old_fee, 1.5), 9000);
    }

    #[test]
    fn test_can_replace_normal_load() {
        let policy = RbfPolicy::new(1000, 0.10);
        let old_fee = 5000;
        assert!(policy.can_replace(6000, old_fee, 1.0));
        assert!(policy.can_replace(7000, old_fee, 1.0));
        assert!(!policy.can_replace(5999, old_fee, 1.0));
    }

    #[test]
    fn test_can_replace_high_load() {
        let policy = RbfPolicy::new(1000, 0.10);
        let old_fee = 5000;
        assert!(policy.can_replace(9000, old_fee, 1.5));
        assert!(!policy.can_replace(8999, old_fee, 1.5));
    }
}
