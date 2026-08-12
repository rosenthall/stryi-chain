//! Stores unconfirmed transactions, their dependencies, and RBF state.

use crate::transactions::{FeeCalculator, FeePolicy};

mod rbf_conflicts;
pub use rbf_conflicts::*;

mod validator;
pub use validator::MempoolValidationError;
mod error;
pub use error::MemPoolError;
mod types;
pub use types::{MemPoolConfig, MemPoolSyncData, MemPoolSyncEntry};

mod dependencies;
mod storage;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::block::BlockData;
use crate::mempool::dependencies::DependencyTracker;
use crate::mempool::storage::TransactionStorage;
use crate::mempool::validator::MempoolTxValidator;
use crate::transactions::{OutPoint, Transaction, TransactionHash, UTXO};
use tracing::warn;

/// Async lookup used to resolve UTXOs that are not produced by the mempool.
pub type UtxoLookup =
    Box<dyn Fn(&OutPoint) -> Pin<Box<dyn Future<Output = Option<UTXO>> + Send>> + Send + Sync>;

/// Mempool state and the helpers needed to maintain it.
pub struct MemPool {
    storage: TransactionStorage,
    dependency_tracker: DependencyTracker,
    rbf_resolver: RbfConflictResolver,
    validator: MempoolTxValidator,
    fee_calculator: FeeCalculator,
    config: MemPoolConfig,
}

impl MemPool {
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

    /// Validates and inserts a transaction.
    pub async fn add_transaction(&mut self, tx: Transaction) -> Result<(), MemPoolError> {
        let tx_hash = tx.data.hash();

        if self.storage.exists(&tx_hash) {
            return Err(MemPoolError::DuplicateTransaction { hash: tx_hash });
        }

        if self.storage.len() >= self.config.max_size {
            return Err(MemPoolError::PoolFull {
                size: self.config.max_size,
            });
        }

        let utxos = match self.validator.validate(&tx, &self.storage).await {
            Ok(utxos) | Err(MempoolValidationError::PotentialRbf(utxos)) => utxos,
            Err(err) => return Err(MemPoolError::ValidationError(err)),
        };

        let actual_fee = self.actual_fee(&tx, &utxos);
        let conflicts = self.rbf_resolver.find_conflicts(&tx, &self.storage);

        if !conflicts.is_empty() {
            if self.config.rbf_policy.is_disabled() {
                warn!("RBF is disabled, rejecting conflicting transaction: {tx_hash}");
                return Err(self
                    .first_conflicting_outpoint(&tx)
                    .map(MemPoolError::DoubleSpend)
                    .unwrap_or(MemPoolError::DuplicateTransaction { hash: tx_hash }));
            }

            self.rbf_resolver
                .resolve_conflicts(
                    &conflicts,
                    &mut self.storage,
                    &mut self.dependency_tracker,
                    actual_fee,
                    1.0,
                )
                .map_err(|err| match err {
                    RbfConflictError::InsufficientFee { required, actual } => {
                        MemPoolError::InsufficientFee { required, actual }
                    }
                    RbfConflictError::Other(message) => {
                        MemPoolError::Storage(Box::new(std::io::Error::other(message)))
                    }
                })?;
        }

        self.storage.insert(tx_hash, tx.clone(), actual_fee);
        let parent_hashes = self.parent_hashes(&tx);
        self.dependency_tracker
            .add_transaction(tx_hash, &parent_hashes);

        Ok(())
    }

    /// Removes a transaction and any descendants that depend on its outputs.
    pub fn remove_transaction(&mut self, tx_hash: TransactionHash) {
        let descendants = self.dependency_tracker.get_descendants(&tx_hash);

        for descendant in descendants {
            self.remove_stored_transaction(&descendant);
        }

        self.remove_stored_transaction(&tx_hash);
    }

    /// Builds a serializable snapshot of the current mempool contents.
    pub fn get_sync_state(&self) -> MemPoolSyncData {
        let entries = self
            .storage
            .get_all()
            .into_iter()
            .map(|entry| MemPoolSyncEntry {
                transaction: entry.transaction.clone(),
                fee: entry.fee,
                timestamp: entry.timestamp,
            })
            .collect();

        MemPoolSyncData {
            entries,
            timestamp: current_timestamp(),
        }
    }

    pub fn get_transaction(&self, tx_hash: &TransactionHash) -> Option<Transaction> {
        self.storage
            .get(tx_hash)
            .map(|entry| entry.transaction.clone())
    }

    /// Restores storage and dependency state from a serialized snapshot.
    pub fn restore_state(&mut self, data: Vec<u8>) -> Result<(), MemPoolError> {
        let sync_data: MemPoolSyncData =
            postcard::from_bytes(&data)
                .map_err(|e| MemPoolError::Storage(Box::new(e)))?;

        self.storage.clear();
        self.dependency_tracker.clear();

        let mut transactions = Vec::with_capacity(sync_data.entries.len());
        for entry in sync_data.entries {
            let tx_hash = entry.transaction.data.hash();
            self.storage.insert_with_timestamp(
                tx_hash,
                entry.transaction.clone(),
                entry.fee,
                entry.timestamp,
            );
            transactions.push(entry.transaction);
        }

        for tx in transactions {
            let tx_hash = tx.data.hash();
            let parent_hashes = self.parent_hashes(&tx);
            self.dependency_tracker
                .add_transaction(tx_hash, &parent_hashes);
        }

        Ok(())
    }

    /// Removes transactions that were confirmed in a block and leaves their children in place.
    pub fn update_on_block(&mut self, block: BlockData) {
        for tx in block.transactions {
            self.remove_stored_transaction(&tx.data.hash());
        }
    }

    /// Removes transactions older than `config.expiry_time`.
    pub fn cleanup_expired(&mut self) {
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
            self.remove_transaction(tx_hash);
        }
    }

    /// Returns up to `limit` transactions in a valid inclusion order.
    ///
    /// Transactions are ranked by the fee rate of the package needed to include them:
    /// the transaction itself plus any unconfirmed ancestors.
    pub fn get_best_transactions(&self, limit: usize) -> Vec<Transaction> {
        if limit == 0 {
            return Vec::new();
        }

        let order = self.dependency_tracker.topological_order();
        let positions: HashMap<TransactionHash, usize> = order
            .iter()
            .enumerate()
            .map(|(idx, hash)| (*hash, idx))
            .collect();

        let mut ranked: Vec<(TransactionHash, f64)> = order
            .iter()
            .map(|hash| (*hash, self.package_fee_rate(*hash)))
            .collect();

        ranked.sort_by(|(left_hash, left_rate), (right_hash, right_rate)| {
            right_rate
                .partial_cmp(left_rate)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    let left_pos = positions.get(left_hash).copied().unwrap_or(usize::MAX);
                    let right_pos = positions.get(right_hash).copied().unwrap_or(usize::MAX);
                    left_pos.cmp(&right_pos)
                })
        });

        let mut selected = HashSet::new();
        let mut result = Vec::new();

        for (tx_hash, _) in ranked {
            if selected.contains(&tx_hash) {
                continue;
            }

            let package: Vec<TransactionHash> = self
                .dependency_tracker
                .get_with_ancestors(&tx_hash)
                .into_iter()
                .filter(|hash| !selected.contains(hash))
                .collect();

            if package.len() > limit.saturating_sub(result.len()) {
                continue;
            }

            for hash in package {
                if selected.insert(hash)
                    && let Some(entry) = self.storage.get(&hash)
                {
                    result.push(entry.transaction.clone());
                }
            }

            if result.len() == limit {
                break;
            }
        }

        result
    }

    pub fn transaction_count(&self) -> usize {
        self.storage.len()
    }

    fn actual_fee(&self, tx: &Transaction, utxos: &[UTXO]) -> u64 {
        let explicit_fee = self.fee_calculator.calculate_fee(tx);
        let input_sum = utxos.iter().map(|utxo| utxo.value).sum::<u64>();
        let output_sum = tx.data.outputs.iter().map(|out| out.value).sum::<u64>();
        let implicit_fee = input_sum.saturating_sub(output_sum);
        explicit_fee.max(implicit_fee)
    }

    fn package_fee_rate(&self, tx_hash: TransactionHash) -> f64 {
        let mut total_fee = 0u64;
        let mut total_size = 0usize;

        for hash in self.dependency_tracker.get_with_ancestors(&tx_hash) {
            if let Some(entry) = self.storage.get(&hash) {
                total_fee = total_fee.saturating_add(entry.fee);
                total_size = total_size.saturating_add(entry.serialized_size);
            }
        }

        if total_size == 0 {
            0.0
        } else {
            total_fee as f64 / total_size as f64
        }
    }

    fn parent_hashes(&self, tx: &Transaction) -> Vec<TransactionHash> {
        tx.data
            .inputs
            .iter()
            .filter_map(|input| {
                self.storage
                    .get_creating_tx(&input.previous_output)
                    .copied()
            })
            .collect()
    }

    fn first_conflicting_outpoint(&self, tx: &Transaction) -> Option<OutPoint> {
        tx.data.inputs.iter().find_map(|input| {
            self.storage
                .get_spending_tx(&input.previous_output)
                .map(|_| input.previous_output)
        })
    }

    fn remove_stored_transaction(&mut self, tx_hash: &TransactionHash) {
        self.storage.remove(tx_hash);
        self.dependency_tracker.remove_transaction(tx_hash);
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}
