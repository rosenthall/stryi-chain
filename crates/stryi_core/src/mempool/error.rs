use thiserror::Error;
use crate::transactions::{OutPoint, TransactionHash};


/// A definition of errors related to mempool implementation
#[derive(Debug, Error)]
pub enum MemPoolError {
    #[error("Pool is full ({size} transactions)")]
    PoolFull { size: usize },

    #[error("Duplicate transaction")]
    DuplicateTransaction { hash: TransactionHash },

    #[error("Insufficient fee (required: {required}, provided: {actual})")]
    InsufficientFee { required: u64, actual: u64 },

    #[error("Missing UTXOs: {0:?}")]
    MissingUtxos(Vec<OutPoint>),

    #[error("Storage error: {0}")]
    Storage(#[from] Box<dyn std::error::Error + Send + Sync>),
}