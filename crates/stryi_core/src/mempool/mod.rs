//! Mempool module that stores unconfirmed transactions, manages dependencies,
//! and provides features such as Replace-by-Fee (RBF), topological ordering,
//! and ancestor scoring for transaction selection.

use crate::transactions::{FeeCalculator, FeePolicy};

mod rbf_conflicts;
pub use rbf_conflicts::*;

mod validator;
mod error;
pub use error::MemPoolError;
mod types;
pub use types::{MemPoolConfig, MemPoolSyncData};

mod storage;
mod dependencies;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use petgraph::graph::NodeIndex;
use crate::block::BlockData;
use crate::mempool::dependencies::DependencyTracker;
use crate::mempool::storage::TransactionStorage;
use crate::mempool::validator::{MempoolTxValidator, MempoolValidationError};
use crate::transactions::{OutPoint, Transaction, TransactionHash, UTXO};

/// A type alias for the asynchronous UTXO lookup function.
/// Given an OutPoint, returns a Future resolving to Option<UTXO>.
pub type UtxoLookup = Box<dyn Fn(&OutPoint) -> Pin<Box<dyn Future<Output = Option<UTXO>> + Send>> + Send + Sync>;

/// Main mempool structure.
pub struct MemPool {
    /// Storage for unconfirmed transactions.
    storage: TransactionStorage,
    /// Dependency tracker that maintains a DAG of transaction dependencies.
    dependency_tracker: DependencyTracker,
    /// Internal conflict resolver using Replace-by-Fee.
    rbf_resolver: RbfConflictResolver,
    /// Validator for incoming transactions.
    validator: MempoolTxValidator,
    /// Fee calculator (based on FeePolicy).
    fee_calculator: FeeCalculator,
    /// General configuration (max_size, fee_policy, rbf_policy, expiry_time, etc.).
    config: MemPoolConfig,
}

impl MemPool {
    /// Creates a new mempool instance.
    ///
    /// Initializes internal modules:
    ///  - TransactionStorage for storing transactions.
    ///  - DependencyTracker for maintaining the dependency graph.
    ///  - FeeCalculator (from config).
    ///  - RbfConflictResolver (from config).
    ///  - MempoolTxValidator with the provided async UTXO lookup.
    pub fn new(config: MemPoolConfig, utxo_lookup: UtxoLookup) -> Self {
        Self {
            storage: TransactionStorage::default(),
            dependency_tracker: DependencyTracker::default(),
            fee_calculator: FeeCalculator::new(config.fee_policy.clone()),
            rbf_resolver: RbfConflictResolver::new(config.rbf_policy.clone()),
            validator: MempoolTxValidator::new(utxo_lookup),
            config,
        }
    }

    /// Adds a new transaction to the mempool.
    ///
    /// 1. Checks if the transaction is already in the pool or if the pool is full.
    /// 2. Validates the transaction using MempoolTxValidator.
    /// 3. Calculates the required fee.
    /// 4. Finds conflicts via `rbf_resolver.find_conflicts(...)`.
    /// 5. Attempts to resolve them with `rbf_resolver.resolve_conflicts(...)`,
    ///    which may remove conflicting transactions and their descendants if the new fee is sufficient.
    /// 6. Inserts the new transaction into storage.
    /// 7. Updates the dependency tracker with parent–child relationships.
    pub async fn add_transaction(&mut self, tx: Transaction) -> Result<(), MemPoolError> {
        let tx_hash = tx.data.hash();

        // 1. Check for duplicate or pool capacity
        if self.storage.exists(&tx_hash) {
            return Err(MemPoolError::DuplicateTransaction { hash: tx_hash });
        }

        if self.storage.len() >= self.config.max_size {
            return Err(MemPoolError::PoolFull { size: self.config.max_size });
        }

        // 2. Validate the transaction (signatures, UTXO ownership, etc.)
        let validation_result = self.validator.validate(&tx, &self.storage).await;
        let utxos = match validation_result {
            Ok(utxos) => utxos,
            Err(MempoolValidationError::PotentialRbf(utxos)) => {
                // This is a special case - it passed all validation except for potentially replacing existing tx
                // We'll proceed with conflict resolution
                utxos
            }
            Err(err) => return Err(MemPoolError::ValidationError(err)),
        };

        // 3. Calculate both explicit and implicit fees
        let explicit_fee = self.fee_calculator.calculate_fee(&tx);

        // Calculate implicit fee (input sum - output sum)
        let input_sum = utxos.iter().map(|utxo| utxo.value).sum::<u64>();
        let output_sum = tx.data.outputs.iter().map(|out| out.value).sum::<u64>();
        let implicit_fee = input_sum.saturating_sub(output_sum);

        // Use the greater of the two fees
        let actual_fee = std::cmp::max(explicit_fee, implicit_fee);

        // For now, we can hardcode a load_factor of 1.0
        let load_factor = 1.0;

        // 4. Find conflicts
        let conflicts = self.rbf_resolver.find_conflicts(&tx, &self.storage);

        // 5. If there are conflicts, resolve them via RBF
        if !conflicts.is_empty() {
            // Process RBF conflicts using the actual fee
            if let Err(rbf_err) = self.rbf_resolver.resolve_conflicts(
                &conflicts,
                &mut self.storage,
                &mut self.dependency_tracker,
                actual_fee, // Use actual_fee here
                load_factor,
            ) {
                return match rbf_err {
                    // If the new fee is too low
                    RbfConflictError::InsufficientFee { required, actual } => {
                        Err(MemPoolError::InsufficientFee { required, actual })
                    }
                    // Fallback for any other error
                    RbfConflictError::Other(msg) => {
                        Err(MemPoolError::Storage(Box::new(
                            std::io::Error::new(std::io::ErrorKind::Other, msg),
                        )))
                    }
                }
            }
        }

        // 6. Insert the new transaction with the actual fee
        self.storage.insert(tx_hash.clone(), tx.clone(), actual_fee);

        // 7. Update dependency tracker with parent-child relationships
        let parent_hashes: Vec<TransactionHash> = tx
            .data
            .inputs
            .iter()
            .filter_map(|input| self.storage.get_creating_tx(&input.previous_output).cloned())
            .collect();

        self.dependency_tracker.add_transaction(tx_hash, &parent_hashes);

        Ok(())
    }

    
    /// Removes a transaction (and its dependent transactions) from the mempool.
    ///
    /// Uses the DependencyTracker to obtain descendant transactions.
    pub async fn remove_transaction(&mut self, tx_hash: TransactionHash) -> Result<(), MemPoolError> {
        // Get descendants before removing the transaction
        let descendants = self.dependency_tracker.get_descendants(&tx_hash);

        // Remove each descendant
        for child_hash in &descendants {
            // Storage.remove now handles all index cleaning properly
            self.storage.remove(child_hash);
            self.dependency_tracker.remove_transaction(child_hash);
        }

        // Remove the transaction itself
        self.storage.remove(&tx_hash);
        self.dependency_tracker.remove_transaction(&tx_hash);

        Ok(())
    }

    /// Serializes the mempool state for network synchronization or persistence.
    ///
    /// Builds a MemPoolSyncData struct containing all transactions and the current timestamp,
    /// then serializes it using bincode.
    pub async fn get_sync_state(&self) -> Result<MemPoolSyncData, MemPoolError> {
        let all_tx = self.storage.get_all();
        let sync_data = MemPoolSyncData {
            transactions: all_tx.iter().map(|entry| entry.transaction.clone()).collect(),
            timestamp: current_timestamp(),
        };

        Ok(sync_data)
    }

    /// Restores the mempool state from a serialized snapshot.
    ///
    /// Clears current storage and dependency tracker, then re-inserts transactions from the snapshot.
    pub async fn restore_state(&mut self, data: Vec<u8>) -> Result<(), MemPoolError> {
        let sync_data: MemPoolSyncData = bincode::serde::decode_from_slice(&data, bincode::config::standard())
            .map_err(|e| MemPoolError::Storage(Box::new(e)))?
            .0;

        self.storage.clear();
        self.dependency_tracker.clear();

        for tx in sync_data.transactions {
            let tx_hash = tx.data.hash();
            let fee = self.fee_calculator.calculate_fee(&tx);

            self.storage.insert(tx_hash.clone(), tx.clone(), fee);

            let parent_hashes: Vec<TransactionHash> = tx
                .data
                .inputs
                .iter()
                .filter_map(|input| self.storage.get_creating_tx(&input.previous_output).cloned())
                .collect();

            self.dependency_tracker.add_transaction(tx_hash, &parent_hashes);
        }

        Ok(())
    }

    /// Updates the mempool after a block is confirmed.
    ///
    /// For every transaction in the block, removes it (and its descendants) from the mempool.
    pub async fn update_on_block(&mut self, block: BlockData) -> Result<(), MemPoolError> {
        for tx in block.transactions {
            let tx_hash = tx.data.hash();
            self.remove_transaction(tx_hash).await?;
        }
        Ok(())
    }

    /// Removes expired transactions from the mempool.
    ///
    /// Checks each stored transaction's timestamp and removes it (and its descendants)
    /// if its age exceeds the configured expiry time.
    pub async fn cleanup_expired(&mut self) -> Result<(), MemPoolError> {
        let now = current_timestamp();
        let expired: Vec<TransactionHash> = self
            .storage
            .get_all()
            .into_iter()
            .filter_map(|entry| {
                if now.saturating_sub(entry.timestamp) > self.config.expiry_time {
                    Some(entry.transaction.data.hash())
                } else {
                    None
                }
            })
            .collect();

        for tx_hash in expired {
            self.remove_transaction(tx_hash).await?;
        }

        Ok(())
    }

    /// Retrieves up to `limit` best transactions for block inclusion.
    ///
    /// Uses a sophisticated selection algorithm that balances between:
    /// - package fee rate (transaction and its ancestors)
    /// - individual fee rate
    /// - dependency constraints
    /// Returns transactions in valid inclusion order (parents before children).
    pub async fn get_best_transactions(&self, limit: usize) -> Result<Vec<Transaction>, MemPoolError> {
        // Get topological ordering of transactions
        let order = self.dependency_tracker.topological_order();

        // Calculate individual fee rates for each transaction
        let mut individual_scores: HashMap<NodeIndex, f64> = HashMap::new();
        for &node_idx in &order {
            if let Some(tx_hash) = self.dependency_tracker.get_tx_by_node(node_idx) {
                if let Some(mem_tx) = self.storage.get(&tx_hash) {
                    let fee_rate = if mem_tx.serialized_size > 0 {
                        mem_tx.fee as f64 / mem_tx.serialized_size as f64
                    } else {
                        0.0
                    };
                    individual_scores.insert(node_idx, fee_rate);
                }
            }
        }

        // Calculate ancestor package scores
        let ancestor_scores = build_ancestor_scores(&order, &self.storage, &self.dependency_tracker);

        // Create combined score using weighted approach
        let mut combined_scores: Vec<(NodeIndex, f64)> = Vec::new();
        for (node_idx, package_score) in ancestor_scores {
            let individual_score = individual_scores.get(&node_idx).copied().unwrap_or(0.0);

            // Weighted combination (can be tuned based on blockchain economics)
            // Higher weight on package score ensures dependencies are preserved
            let combined_score = (package_score * 0.7) + (individual_score * 0.3);

            combined_scores.push((node_idx, combined_score));
        }

        // Sort by combined score
        combined_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Select transactions with knapsack-like approach
        let mut result = Vec::new();
        let mut used_nodes = HashSet::new();
        let mut remaining_space = limit;

        for (node_idx, _) in combined_scores {
            // Skip if already included or no space left
            if used_nodes.contains(&node_idx) || remaining_space == 0 {
                continue;
            }

            if let Some(tx_hash) = self.dependency_tracker.get_tx_by_node(node_idx) {
                if let Some(_) = self.storage.get(&tx_hash) {
                    // Get all required ancestors in topological order
                    let ancestors = self.dependency_tracker.gather_ancestors(node_idx);

                    // Count new transactions (not already selected)
                    let new_txs: Vec<NodeIndex> = ancestors.into_iter()
                        .filter(|&anc| !used_nodes.contains(&anc))
                        .collect();

                    // Check if all ancestors fit in remaining space
                    if new_txs.len() <= remaining_space {
                        for anc in new_txs {
                            if used_nodes.insert(anc) {
                                if let Some(anc_tx_hash) = self.dependency_tracker.get_tx_by_node(anc) {
                                    if let Some(anc_tx) = self.storage.get(&anc_tx_hash) {
                                        result.push(anc_tx.transaction.clone());
                                        remaining_space -= 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(result)
    }
}

/// Helper function to get current Unix timestamp
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}

/// Computes ancestor fee scores for each node in the dependency graph.
/// For each node, sums fees and serialized sizes for itself and all its ancestors,
/// then computes a fee rate as total_fee/total_size.
fn build_ancestor_scores(
    order: &[NodeIndex],
    storage: &TransactionStorage,
    tracker: &DependencyTracker,
) -> Vec<(NodeIndex, f64)> {
    let mut scores = Vec::new();
    for &node in order {
        let ancestors = tracker.gather_ancestors(node);
        let mut total_fee = 0u64;
        let mut total_size = 0usize;

        for anc in ancestors {
            if let Some(tx_hash) = tracker.get_tx_by_node(anc) {
                if let Some(mem_tx) = storage.get(&tx_hash) {
                    total_fee = total_fee.saturating_add(mem_tx.fee);
                    // Use cached size instead of recalculating
                    total_size = total_size.saturating_add(mem_tx.serialized_size);
                }
            }
        }

        let score = if total_size == 0 { 0.0 } else { total_fee as f64 / total_size as f64 };
        scores.push((node, score));
    }
    scores
}