use crate::transactions::Transaction;

/// Rough size estimate for a serialized transaction.
/// Used for pre-transaction fee estimation when no Transaction object exists yet.
const BASE_SIZE: usize = 91;

/// Rough size of a single serialized `TransactionIn`.
const SINGLE_INPUT_SIZE: usize = 69;

/// Rough size of a single serialized `TransactionOut`.
const SINGLE_OUTPUT_SIZE: usize = 43;

/// Vector length overhead in serialization (varint prefix).
const VEC_LEN_OVERHEAD: usize = 1;

/// Roughly estimate serialized transaction size in bytes using just `num_inputs` and `num_outputs`.
pub fn estimate_transaction_size(num_inputs: usize, num_outputs: usize) -> usize {
    let mut size = BASE_SIZE;

    size += num_inputs * SINGLE_INPUT_SIZE;
    size += num_outputs * SINGLE_OUTPUT_SIZE;

    if num_inputs > 0 || num_outputs > 0 {
        size += VEC_LEN_OVERHEAD;
    }

    if num_outputs > 1 {
        size += (num_outputs - 1) * 2;
    }

    size
}

impl Transaction {
    /// Returns the exact serialized size of this transaction.
    pub fn serialized_size(&self) -> usize {
        postcard::to_stdvec(self)
            .map(|v| v.len())
            .expect("Transaction serialization cannot fail")
    }
}
