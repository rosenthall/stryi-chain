use std::collections::HashSet;
use thiserror::Error;
use crate::mempool::dependencies::DependencyTracker;
use crate::transactions::{Transaction, TransactionHash};
use crate::mempool::storage::TransactionStorage;

/// ConflictError enumerates possible errors during conflict resolution (e.g., RBF checks).
#[derive(Debug, Error)]
pub enum RbfConflictError {
    #[error("New transaction fee is too low to replace the existing ones. Required: {required}, got: {actual}")]
    InsufficientFee {
        required: u64,
        actual: u64,
    },

    #[error("Conflict resolution failed: {0}")]
    Other(String),
}

/// RbfPolicy defines parameters for an adaptive Replace-By-Fee (RBF) policy.
#[derive(Debug, Clone)]
pub struct RbfPolicy {
    /// Minimum absolute fee increase (in satoshis).
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
    /// Creates a new RbfPolicy with given parameters.
    pub fn new(base_fee_delta: u64, percentage_increase: f64) -> Self {
        Self {
            base_fee_delta,
            percentage_increase,
        }
    }

    /// Computes the required fee to replace an existing transaction fee under a given load factor.
    pub fn required_fee(&self, old_fee: u64, load_factor: f64) -> u64 {
        // Calculate required fee by absolute increase.
        let required_absolute = old_fee.saturating_add(self.base_fee_delta);
        // Calculate required fee by percentage increase.
        let required_percentage = ((old_fee as f64) * (1.0 + self.percentage_increase)).ceil() as u64;
        // Base required fee is the maximum of the two.
        let base_required_fee = std::cmp::max(required_absolute, required_percentage);
        // Adjust requirement based on the current load factor.
        ((base_required_fee as f64) * load_factor).ceil() as u64
    }

    /// Determines if a new fee can replace an old fee under the given load factor.
    pub fn can_replace(&self, new_fee: u64, old_fee: u64, load_factor: f64) -> bool {
        new_fee >= self.required_fee(old_fee, load_factor)
    }
}

/// RbfConflictResolver is responsible for detecting conflicting transactions and determining
/// if a new transaction can replace existing ones using an adaptive RBF policy.
pub struct RbfConflictResolver {
    policy: RbfPolicy,
}

impl RbfConflictResolver {
    /// Creates a new RbfConflictResolver with the given RbfPolicy.
    pub fn new(policy: RbfPolicy) -> Self {
        Self { policy }
    }

    /// Scans the inputs of `new_tx` to detect any conflicting transactions in storage.
    /// Returns a set of conflicting transaction hashes.
    pub fn find_conflicts(
        &self,
        new_tx: &Transaction,
        storage: &TransactionStorage,
    ) -> HashSet<TransactionHash> {
        let mut conflicts = HashSet::new();

        // Check each input of the new transaction
        for inp in &new_tx.data.inputs {
            // See if any transaction in the mempool is already spending this outpoint
            if let Some(existing_hash) = storage.get_spending_tx(&inp.previous_output) {
                conflicts.insert(existing_hash.clone());
            }
        }

        conflicts
    }

    /// Resolves conflicts by checking if the new transaction fee is sufficient to replace
    /// the existing conflicting transactions according to the adaptive RBF policy.
    /// Supports replacing multiple conflicting transactions.
    ///
    /// Parameters:
    /// - `conflicts`: set of conflicting transaction hashes.
    /// - `storage`: mutable reference to TransactionStorage.
    /// - `tracker`: mutable reference to DependencyTracker.
    /// - `new_fee`: fee of the new transaction.
    /// - `load_factor`: current mempool load factor.
    ///
    /// Returns Ok(()) if replacement is allowed, otherwise returns a ConflictError.
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

        // Calculate combined fees of all conflicting transactions
        let mut total_conflict_fee = 0u64;
        let mut all_affected_txs = HashSet::new();

        // First, calculate fees of directly conflicting transactions
        for conflict_hash in conflicts {
            if let Some(conflict_tx) = storage.get(conflict_hash) {
                total_conflict_fee = total_conflict_fee.saturating_add(conflict_tx.fee);
                all_affected_txs.insert(conflict_hash.clone());

                // Also include all descendants of conflicting transactions
                let descendants = tracker.get_descendants(conflict_hash);
                for desc_hash in &descendants {
                    all_affected_txs.insert(desc_hash.clone());

                    if let Some(desc_tx) = storage.get(desc_hash) {
                        total_conflict_fee = total_conflict_fee.saturating_add(desc_tx.fee);
                    }
                }
            }
        }

        // Calculate required fee with a premium based on the number of conflicts
        let conflict_count_factor = 1.0 + (conflicts.len() as f64 * 0.05); // 5% premium per conflict
        let base_required_fee = self.policy.required_fee(total_conflict_fee, load_factor);
        let required_fee = (base_required_fee as f64 * conflict_count_factor).ceil() as u64;

        if new_fee < required_fee {
            return Err(RbfConflictError::InsufficientFee {
                required: required_fee,
                actual: new_fee,
            });
        }

        // Sort affected transactions by dependency depth to remove descendants first
        let mut affected_by_depth: Vec<&TransactionHash> = all_affected_txs.iter().collect();
        affected_by_depth.sort_by(|a, b| {
            let a_deps = tracker.get_descendants(a).len();
            let b_deps = tracker.get_descendants(b).len();
            b_deps.cmp(&a_deps) // Reverse order (most dependencies first)
        });

        // Remove all affected transactions
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
        // Parameters: +1000 satoshi, 10% increase.
        let policy = RbfPolicy::new(1000, 0.10);
        let old_fee = 5000;
        // required_absolute = 5000 + 1000 = 6000
        // required_percentage = ceil(5000 * 1.10) = 5500
        // base_required_fee = max(6000, 5500) = 6000
        // load_factor = 1.0, adjusted_required_fee = 6000 * 1.0 = 6000
        assert_eq!(policy.required_fee(old_fee, 1.0), 6000);
    }

    #[test]
    fn test_required_fee_high_load() {
        // For high load with load_factor = 1.5.
        let policy = RbfPolicy::new(1000, 0.10);
        let old_fee = 5000;
        // base_required_fee = 6000, adjusted_required_fee = 6000 * 1.5 = 9000
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
        // required_fee = 6000 * 1.5 = 9000
        assert!(policy.can_replace(9000, old_fee, 1.5));
        assert!(!policy.can_replace(8999, old_fee, 1.5));
    }
}