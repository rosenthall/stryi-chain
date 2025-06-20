use std::collections::HashSet;
use crate::BlockUndo;
use crate::storage::UtxoStorage;
use crate::transactions::{Transaction, TransactionKind, OutPoint, UTXO};

/// UtxoProcessor is responsible for applying and reverting blocks to the UTXO set.
// TODO: Add some configuration structs?
pub struct UtxoProcessor;

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
    pub async fn apply_block<S>(
        &self,
        block: &crate::block::Block,
        utxo_storage: &mut S,
    ) -> Result<BlockUndo, S::StorageError>
    where
        S: UtxoStorage + Send,
    {
        let mut created_outpoints : HashSet<OutPoint>  = HashSet::new();
        let mut spent_utxos: HashSet<(OutPoint, UTXO)> = HashSet::new();

        for tx in &block.data.transactions {
            match tx.data.kind {
                TransactionKind::Genesis | TransactionKind::Coinbase => {
                    created_outpoints.extend(self.put_outputs(tx, utxo_storage).await?);
                }
                TransactionKind::Payment => {
                    // collect and remove inputs
                    let in_ops: Vec<_> = tx
                        .data
                        .inputs
                        .iter()
                        .map(|i| i.previous_output)
                        .collect();

                    // fetch current UTXOs so we can store them in undo
                    let existing=
                        utxo_storage.batch_get_utxos(in_ops.clone()).await?;

                    for op in &in_ops {
                        if let Some(u) = existing.get(op) {
                            spent_utxos.insert((*op, *u));
                        }
                    }

                    utxo_storage.batch_remove_utxos(in_ops).await?;

                    // add outputs
                    created_outpoints.extend(self.put_outputs(tx, utxo_storage).await?);
                }
            }
        }

        Ok(BlockUndo {
            spent_utxos,
            created_outpoints
        })
    }



    
    /// Helper, inserts transaction outputs, returns list of newly created `OutPoint`s.
    async fn put_outputs<S>(
        &self,
        tx: &Transaction,
        utxo_storage: &mut S,
    ) -> Result<Vec<OutPoint>, S::StorageError>
    where
        S: UtxoStorage + Send,
    {
        let txid = tx.data.hash();

        let mut batch: Vec<(OutPoint, UTXO)> = Vec::with_capacity(tx.data.outputs.len());
        let mut outpoints: Vec<OutPoint>     = Vec::with_capacity(tx.data.outputs.len());

        for (vout, output) in tx.data.outputs.iter().enumerate() {
            let op = OutPoint { txid, vout: vout as u32 };
            let utxo = UTXO { txid, vout: op.vout, value: output.value, owner: output.recipient };
            batch.push((op, utxo));
            outpoints.push(op);
        }

        utxo_storage.batch_put_utxos(batch).await?;
        Ok(outpoints)
    }
    

    /// Reverts a previously-applied block, restoring the UTXO set to its prior state.
    pub async fn rewind_block<S>(
        &self,
        undo: BlockUndo,
        utxo_storage: &mut S,
    ) -> Result<(), S::StorageError>
    where
        S: UtxoStorage + Send,
    {
        // return all spent outputs
        let spent_outputs = undo.spent_utxos.iter().cloned().collect::<Vec<_>>();
        utxo_storage.batch_put_utxos(spent_outputs).await?;
        
        
        // remove outputs that were created by the reverted block
        let created_outputs = undo.created_outpoints.iter().cloned().collect::<Vec<_>>();
        utxo_storage.batch_remove_utxos(created_outputs).await
    }
}
