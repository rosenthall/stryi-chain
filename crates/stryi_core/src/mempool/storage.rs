use std::collections::HashMap;
use crate::mempool::current_timestamp;
use crate::transactions::{Transaction, TransactionHash, OutPoint};
use crate::mempool::types::MemPoolTx;
use bincode::config::standard;

/// TransactionStorage is a container for mempool transactions with
/// separate indices for created and spent outpoints.
///
/// # Responsibilities
/// - Store `MemPoolTx` entries keyed by their `TransactionHash`.
/// - Track which transactions created outpoints (`output_creation_index`).
/// - Track which transactions spend outpoints (`input_spending_index`).
/// - Provide methods to insert and remove transactions, automatically
///   maintaining indices consistency.
#[derive(Default)]
pub struct TransactionStorage {
    /// A map from transaction hash to the stored mempool transaction data.
    pub(crate) transactions: HashMap<TransactionHash, MemPoolTx>,

    /// A map from outpoints to the transaction hash that created them.
    output_creation_index: HashMap<OutPoint, TransactionHash>,

    /// A map from outpoints to the transaction hash that spends them.
    input_spending_index: HashMap<OutPoint, TransactionHash>,
}


impl TransactionStorage {
    /// Inserts a transaction into the storage.
    /// - tx_hash: the hash of the transaction (e.g., tx.data.hash()).
    /// - tx: the Transaction itself
    /// - fee: computed fee for the transaction
    /// This method stores the entry in `transactions` and manages both output and input indices.
    pub fn insert(&mut self, tx_hash: TransactionHash, tx: Transaction, fee: u64) {
        // Calculate serialized size once during insertion
        let serialized_size = bincode::serde::encode_to_vec(&tx, standard())
            .expect("Transaction serialization cannot fail")
            .len();

        let mem_tx = MemPoolTx {
            transaction: tx.clone(),
            timestamp: current_timestamp(),
            fee,
            serialized_size,
        };

        // Insert the new entry
        self.transactions.insert(tx_hash, mem_tx);

        // Index outputs (this transaction CREATES these outpoints)
        for (vout_idx, _) in tx.data.outputs.iter().enumerate() {
            let op = OutPoint {
                txid: tx_hash,
                vout: vout_idx as u32,
            };
            self.output_creation_index.insert(op, tx_hash);
        }

        // Index inputs (this transaction SPENDS these outpoints)
        for input in &tx.data.inputs {
            self.input_spending_index.insert(input.previous_output, tx_hash);
        }
    }

    /// Removes a transaction by hash, returning the removed MemPoolTx if found.
    /// Also, properly updates both output and input indices.
    pub fn remove(&mut self, tx_hash: &TransactionHash) -> Option<MemPoolTx> {
        let removed = self.transactions.remove(tx_hash);

        if let Some(ref mem_tx) = removed {
            // Clean up output creation index (outputs this tx created)
            for (vout_idx, _) in mem_tx.transaction.data.outputs.iter().enumerate() {
                let op = OutPoint {
                    txid: *tx_hash,
                    vout: vout_idx as u32,
                };
                self.output_creation_index.remove(&op);
            }

            // Clean up input spending index (inputs this tx spends)
            for input in &mem_tx.transaction.data.inputs {
                // Only remove if THIS transaction is the one spending it
                if let Some(spending_tx) = self.get_spending_tx(&input.previous_output) {
                    if spending_tx == tx_hash {
                        self.input_spending_index.remove(&input.previous_output);
                    }
                }
            }
        }

        removed
    }

    /// Retrieves an immutable reference to a stored MemPoolTx by hash.
    pub fn get(&self, tx_hash: &TransactionHash) -> Option<&MemPoolTx> {
        self.transactions.get(tx_hash)
    }

    /// Gets all the stored transactions in mempool
    pub fn get_all(&self) -> Vec<&MemPoolTx> {
        self.transactions.values().collect()
    }

    /// Checks if such transaction exists in mempool
    pub fn exists(&self, tx_hash: &TransactionHash) -> bool {
        self.transactions.contains_key(tx_hash)
    }

    /// Retrieves a mutable reference to a stored MemPoolTx by hash.
    pub fn get_mut(&mut self, tx_hash: &TransactionHash) -> Option<&mut MemPoolTx> {
        self.transactions.get_mut(tx_hash)
    }

    /// Returns which transaction created a given outpoint, if any.
    pub fn get_creating_tx(&self, outpoint: &OutPoint) -> Option<&TransactionHash> {
        self.output_creation_index.get(outpoint)
    }

    /// Returns which transaction spends a given outpoint, if any.
    pub fn get_spending_tx(&self, outpoint: &OutPoint) -> Option<&TransactionHash> {
        self.input_spending_index.get(outpoint)
    }

    /// Checks if an outpoint is being spent by any transaction in mempool.
    pub fn is_outpoint_spent(&self, outpoint: &OutPoint) -> bool {
        self.input_spending_index.contains_key(outpoint)
    }

    /// Clears storage state
    pub fn clear(&mut self) {
        self.transactions.clear();
        self.output_creation_index.clear();
        self.input_spending_index.clear();
    }

    /// Returns the total number of transactions in storage.
    pub fn len(&self) -> usize {
        self.transactions.len()
    }

    /// Returns whether the storage is empty
    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }
}