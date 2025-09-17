use crate::mempool::{FeePolicy, RbfPolicy};
use crate::transactions::Transaction;
use serde::{Deserialize, Serialize};

/// Serializable mempool state for network synchronization
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct MemPoolSyncData {
    /// List of all transactions in mempool
    pub(crate) transactions: Vec<Transaction>,
    /// Timestamp when state was created
    pub(crate) timestamp: u64,
}

/// Configuration for mempool behavior and limits
#[derive(Clone, Debug)]
pub struct MemPoolConfig {
    /// Maximum number of transactions in pool
    pub max_size: usize,
    /// Fee calculation policy
    pub fee_policy: FeePolicy,
    /// Replace-by-Fee policy
    pub rbf_policy: RbfPolicy,
    /// Time in seconds after which transaction is considered expired
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
    /// Creates new mempool configuration with custom parameters
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

/// `Transaction` wrapper with mempool-specific metadata
#[derive(Debug)]
pub struct MemPoolTx {
    /// The actual transaction
    pub(crate) transaction: Transaction,
    /// Unix timestamp when transaction was added
    pub(crate) timestamp: u64,
    /// Calculated fee based on inputs/outputs
    pub(crate) fee: u64,
    /// Cached serialized size in bytes
    pub(crate) serialized_size: usize,
}
