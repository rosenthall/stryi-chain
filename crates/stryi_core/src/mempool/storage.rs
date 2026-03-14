use crate::mempool::current_timestamp;
use crate::mempool::types::MemPoolTx;
use crate::transactions::{OutPoint, Transaction, TransactionHash};
use bincode::config::standard;
use std::collections::HashMap;

/// Stores mempool transactions and the indexes needed to resolve dependencies.
#[derive(Default)]
pub struct TransactionStorage {
    transactions: HashMap<TransactionHash, MemPoolTx>,
    output_creation_index: HashMap<OutPoint, TransactionHash>,
    input_spending_index: HashMap<OutPoint, TransactionHash>,
}

impl TransactionStorage {
    /// Inserts a transaction and stamps it with the current time.
    pub fn insert(&mut self, tx_hash: TransactionHash, tx: Transaction, fee: u64) {
        self.insert_with_timestamp(tx_hash, tx, fee, current_timestamp());
    }

    /// Inserts a transaction with an explicit timestamp.
    pub fn insert_with_timestamp(
        &mut self,
        tx_hash: TransactionHash,
        tx: Transaction,
        fee: u64,
        timestamp: u64,
    ) {
        // Calculate serialized size once during insertion
        let serialized_size = bincode::serde::encode_to_vec(&tx, standard())
            .expect("Transaction serialization cannot fail")
            .len();

        let mem_tx = MemPoolTx {
            transaction: tx.clone(),
            timestamp,
            fee,
            serialized_size,
        };

        self.transactions.insert(tx_hash, mem_tx);

        for (vout_idx, _) in tx.data.outputs.iter().enumerate() {
            let op = OutPoint {
                txid: tx_hash,
                vout: vout_idx as u32,
            };
            self.output_creation_index.insert(op, tx_hash);
        }

        // Index inputs (this transaction SPENDS these outpoints)
        for input in &tx.data.inputs {
            self.input_spending_index
                .insert(input.previous_output, tx_hash);
        }
    }

    /// Removes a transaction and cleans up its indexes.
    pub fn remove(&mut self, tx_hash: &TransactionHash) -> Option<MemPoolTx> {
        let removed = self.transactions.remove(tx_hash);

        if let Some(ref mem_tx) = removed {
            for (vout_idx, _) in mem_tx.transaction.data.outputs.iter().enumerate() {
                let op = OutPoint {
                    txid: *tx_hash,
                    vout: vout_idx as u32,
                };
                self.output_creation_index.remove(&op);
            }

            for input in &mem_tx.transaction.data.inputs {
                if let Some(spending_tx) = self.get_spending_tx(&input.previous_output)
                    && spending_tx == tx_hash
                {
                    self.input_spending_index.remove(&input.previous_output);
                }
            }
        }

        removed
    }

    /// Retrieves an immutable reference to a stored MemPoolTx by hash.
    pub fn get(&self, tx_hash: &TransactionHash) -> Option<&MemPoolTx> {
        self.transactions.get(tx_hash)
    }

    /// Returns all stored transactions.
    pub fn get_all(&self) -> Vec<&MemPoolTx> {
        self.transactions.values().collect()
    }

    /// Returns `true` if the transaction is stored.
    pub fn exists(&self, tx_hash: &TransactionHash) -> bool {
        self.transactions.contains_key(tx_hash)
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
}
