use crate::mempool::{FeePolicy, RbfPolicy};
use crate::transactions::Transaction;
use serde::{Deserialize, Serialize};

/// Serialized mempool state used for sync and restore.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MemPoolSyncData {
    /// Transactions and metadata captured from the mempool.
    pub entries: Vec<MemPoolSyncEntry>,
    /// Timestamp when the snapshot was created.
    pub timestamp: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MemPoolSyncEntry {
    pub transaction: Transaction,
    pub fee: u64,
    pub timestamp: u64,
}

#[derive(Clone, Debug)]
pub struct MemPoolConfig {
    /// Maximum number of transactions in the pool.
    pub max_size: usize,
    /// Fee calculation policy.
    pub fee_policy: FeePolicy,
    /// Replace-by-Fee policy.
    pub rbf_policy: RbfPolicy,
    /// Maximum age for `cleanup_expired`.
    pub expiry_time: u64,
}

impl Default for MemPoolConfig {
    fn default() -> Self {
        Self {
            max_size: 5000,
            fee_policy: FeePolicy::default(),
            rbf_policy: RbfPolicy::default(),
            expiry_time: 60 * 60, // 1 hour
        }
    }
}

impl MemPoolConfig {
    /// Creates a mempool configuration with custom parameters.
    pub fn new(
        max_size: usize,
        fee_policy: FeePolicy,
        rbf_policy: RbfPolicy,
        expiry_time: u64,
    ) -> Self {
        Self {
            max_size,
            fee_policy,
            rbf_policy,
            expiry_time,
        }
    }
}

#[derive(Debug)]
pub struct MemPoolTx {
    pub(crate) transaction: Transaction,
    pub(crate) timestamp: u64,
    pub(crate) fee: u64,
    pub(crate) serialized_size: usize,
}
