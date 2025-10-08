// This module heavily relies on information from bincode specs :
// https://git.sr.ht/~stygianentity/bincode/tree/trunk/item/docs/spec.md
// bincode is not that easy as I thought as this point

use crate::transactions::Transaction;

/// Base size of bincode-serialized transaction, includes `version`, `kind` and `signature`, with no ins/outs.
const BASE_SIZE: usize = 91;

/// Size of a single bincode-serialized `TransactionIn` object.
const SINGLE_INPUT_SIZE: usize = 69;

/// Size of a single bincode-serialized `TransactionOut` object.
const SINGLE_OUTPUT_SIZE: usize = 43;

/// Vector length overhead in bincode serialization
const VEC_LEN_OVERHEAD: usize = 4;

/// Overhead bytes on each the output in tx.
const PER_ADDITIONAL_OUTPUT_OVERHEAD: usize = 2;

/// Estimate bincode-serialized transaction size in bytes using just `num_inputs` and `num_outputs`
pub const fn estimate_transaction_size(num_inputs: usize, num_outputs: usize) -> usize {
    // Base transaction structure
    let mut size = BASE_SIZE;

    // Add sizes for all inputs and outputs
    size += num_inputs * SINGLE_INPUT_SIZE;
    size += num_outputs * SINGLE_OUTPUT_SIZE;

    // If there are any inputs or outputs, account for the Vec length prefix
    if num_inputs > 0 || num_outputs > 0 {
        size += VEC_LEN_OVERHEAD;
    }

    // Each additional output beyond the first incurs small overhead
    if num_outputs > 1 {
        size += (num_outputs - 1) * PER_ADDITIONAL_OUTPUT_OVERHEAD;
    }

    size
}

impl Transaction {
    /// Statically estimates the size of this transaction when it will be serialized
    pub const fn estimate_serialized_size(&self) -> usize {
        estimate_transaction_size(self.data.inputs.len(), self.data.outputs.len())
    }
}

#[cfg(test)]
mod test {
    use crate::address::AccountAddress;
    use crate::transactions::{
        OutPoint, TransactionData, TransactionHash, TransactionIn, TransactionKind, TransactionOut,
    };
    use bincode::config::standard;
    use bincode::serde::encode_to_vec;
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::rand_core::OsRng;

    #[test]
    fn test_transaction_size_predictions() {
        // Create random signing key for test transactions
        let signing_key = SigningKey::random(&mut OsRng);

        // Test cases with different input/output combinations
        let test_cases = vec![
            (0, 1),  // Coinbase-like
            (1, 1),  // Simple payment
            (2, 2),  // Multi-input/output
            (15, 1), // Exaggerated utxo consolidation
            (5, 3),  // More complex
            (10, 5), // Large transaction
        ];

        for (num_inputs, num_outputs) in test_cases {
            // Create dummy transaction
            let tx_data = TransactionData {
                version: 1,
                kind: TransactionKind::Payment,
                inputs: (0..num_inputs)
                    .map(|i| TransactionIn {
                        previous_output: OutPoint {
                            txid: TransactionHash::new(&[i as u8; 32]),
                            vout: i as u32,
                        },
                        sequence: 1,
                    })
                    .collect(),
                outputs: (0..num_outputs)
                    .map(|i| TransactionOut {
                        value: 1000 * (i + 1),
                        recipient: AccountAddress::new(&[i as u8; 32]),
                    })
                    .collect(),
            };

            let transaction = tx_data.sign(&signing_key);

            // Get estimated size
            let estimated_size = transaction.estimate_serialized_size();

            // Get actual size by serializing
            let actual_size = encode_to_vec(&transaction, standard()).unwrap().len();

            // Compare with some debug output
            dbg!(num_inputs, num_outputs, estimated_size, actual_size);

            // They should be exactly equal since we know the exact bincode format
            assert_eq!(
                estimated_size, actual_size,
                "Size mismatch for tx with {} inputs and {} outputs",
                num_inputs, num_outputs
            );
        }
    }
}
