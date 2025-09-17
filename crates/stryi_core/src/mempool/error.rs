use crate::mempool::validator::MempoolValidationError;
use crate::transactions::{OutPoint, TransactionHash};
use thiserror::Error;

/// A definition of errors related to mempool implementation
#[derive(Debug, Error)]
pub enum MemPoolError {
    #[error("Pool is full ({size} transactions)")]
    PoolFull { size: usize },

    #[error("Duplicate transaction {hash}")]
    DuplicateTransaction { hash: TransactionHash },

    #[error("Insufficient fee (required: {required}, provided: {actual})")]
    InsufficientFee { required: u64, actual: u64 },

    #[error("Double spend detected: outpoint {0:?} is already used in mempool")]
    DoubleSpend(OutPoint),

    #[error("Cannot validate transaction: {0:?}")]
    ValidationError(MempoolValidationError),

    #[error("Storage error: {0}")]
    Storage(#[from] Box<dyn std::error::Error + Send + Sync>),

    #[error("Concurrent operation error: {0}")]
    ConcurrencyError(String),
}
