use crate::BlockUndo;
use crate::storage::{StorageError, UtxoWrite};
use crate::transactions::{OutPoint, TransactionKind, UTXO};
use std::collections::{HashMap, HashSet};

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
        let mut created_utxos: HashMap<OutPoint, UTXO> = HashMap::new();
        let mut external_inputs: Vec<OutPoint> = Vec::new();

        for tx in &block.data.transactions {
            if tx.data.kind == TransactionKind::Payment {
                for input in &tx.data.inputs {
                    let op = input.previous_output;
                    // An output created and spent in this block has no pre-block state
                    // to restore. Undo records only the net change to the UTXO set.
                    if created_utxos.remove(&op).is_none() {
                        external_inputs.push(op);
                    }
                }
            }

            let txid = tx.data.hash();
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
                created_utxos.insert(op, utxo);
            }
        }

        let mut spent_utxos: HashSet<(OutPoint, UTXO)> =
            HashSet::with_capacity(external_inputs.len());

        if !external_inputs.is_empty() {
            // fetch current UTXOs so we can store them in undo
            let mut existing = utxo_storage.batch_get_utxos(external_inputs.iter().copied())?;
            for &op in &external_inputs {
                let utxo = existing.remove(&op).ok_or(StorageError::UtxoNotFound(op))?;
                spent_utxos.insert((op, utxo));
            }

            utxo_storage.batch_remove_utxos(external_inputs)?;
        }

        let created_outpoints: HashSet<OutPoint> = created_utxos.keys().copied().collect();
        if !created_utxos.is_empty() {
            utxo_storage.batch_put_utxos(created_utxos.into_iter().collect())?;
        }

        Ok(BlockUndo {
            spent_utxos,
            created_outpoints,
        })
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
