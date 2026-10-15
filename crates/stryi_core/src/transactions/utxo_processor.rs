use crate::BlockUndo;
use crate::storage::{StorageError, UtxoWrite};
use crate::transactions::{OutPoint, Transaction, TransactionKind, UTXO};
use std::collections::HashSet;

/// UtxoProcessor is responsible for applying and reverting blocks to the UTXO set.
// TODO: Add some configuration for the UtxoProcessor?
pub struct UtxoProcessor;

impl Default for UtxoProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl UtxoProcessor {
    /// Creates a new UtxoProcessor instance.
    pub fn new() -> Self {
        Self
    }

    /// Applies a validated block to the UTXO storage **and returns** the diff required to undo it.
    ///
    /// This involves processing each transaction in the block:
    /// - Removing consumed UTXOs.
    /// - Adding new UTXOs created by the transaction.
    /// - Collecting `(created, spent)` so the caller can persist them as `BlockUndo`.
    ///
    /// # Returns
    ///
    /// `Ok(BlockUndo)` if the block is successfully applied;
    /// the undo-object can later be fed to `rewind_block` during a re-org.
    pub fn apply_block<S>(
        &self,
        block: &crate::block::Block,
        utxo_storage: &mut S,
    ) -> Result<BlockUndo, StorageError>
    where
        S: UtxoWrite,
    {
        let mut created_outpoints: HashSet<OutPoint> = HashSet::new();
        let mut spent_utxos: HashSet<(OutPoint, UTXO)> = HashSet::new();

        for tx in &block.data.transactions {
            match tx.data.kind {
                TransactionKind::Genesis | TransactionKind::Coinbase { .. } => {
                    created_outpoints.extend(self.put_outputs(tx, utxo_storage)?);
                }
                TransactionKind::Payment => {
                    // collect and remove inputs
                    let in_ops: Vec<_> = tx.data.inputs.iter().map(|i| i.previous_output).collect();

                    // fetch current UTXOs so we can store them in undo
                    let existing = utxo_storage.batch_get_utxos(in_ops.clone())?;

                    for op in &in_ops {
                        // An output created and spent in this block has no pre-block state
                        // to restore. Undo records only the net change to the UTXO set.
                        if !created_outpoints.remove(op) {
                            let utxo = existing.get(op).ok_or(StorageError::UtxoNotFound(*op))?;
                            spent_utxos.insert((*op, *utxo));
                        }
                    }

                    utxo_storage.batch_remove_utxos(in_ops)?;

                    // add outputs
                    created_outpoints.extend(self.put_outputs(tx, utxo_storage)?);
                }
            }
        }

        Ok(BlockUndo {
            spent_utxos,
            created_outpoints,
        })
    }

    /// Helper, inserts transaction outputs, returns list of newly created `OutPoint`s.
    fn put_outputs<S>(
        &self,
        tx: &Transaction,
        utxo_storage: &mut S,
    ) -> Result<Vec<OutPoint>, StorageError>
    where
        S: UtxoWrite,
    {
        let txid = tx.data.hash();

        let mut batch: Vec<(OutPoint, UTXO)> = Vec::with_capacity(tx.data.outputs.len());
        let mut outpoints: Vec<OutPoint> = Vec::with_capacity(tx.data.outputs.len());

        for (vout, output) in tx.data.outputs.iter().enumerate() {
            let op = OutPoint {
                txid,
                vout: vout as u32,
            };
            let utxo = UTXO {
                txid,
                vout: op.vout,
                value: output.value,
                owner: output.recipient,
            };
            batch.push((op, utxo));
            outpoints.push(op);
        }

        utxo_storage.batch_put_utxos(batch)?;
        Ok(outpoints)
    }

    /// Reverts a previously applied block, restoring the UTXO set to its prior state.
    pub fn rewind_block<S>(&self, undo: BlockUndo, utxo_storage: &mut S) -> Result<(), StorageError>
    where
        S: UtxoWrite,
    {
        // Remove this block's outputs before restoring its inputs, so an overlapping
        // outpoint ends with its original value and address membership.
        let created_outputs = undo.created_outpoints.into_iter().collect();
        utxo_storage.batch_remove_utxos(created_outputs)?;

        let spent_outputs = undo.spent_utxos.into_iter().collect();
        utxo_storage.batch_put_utxos(spent_outputs)
    }
}
