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

    /// Applies a validated block to the UTXO storage.
    ///
    /// This involves processing each transaction in the block:
    /// - Removing consumed UTXOs.
    /// - Adding new UTXOs created by the transaction.
    ///
    /// # Parameters
    ///
    /// - `block`: Reference to the block to be applied.
    /// - `utxo_storage`: Mutable reference to the UTXO storage.
    ///
    /// # Returns
    ///
    /// - `Ok(())` if the block is successfully applied.
    /// - `Err(StryiCoreError)` if an error occurs during processing.
    pub async fn apply_block<S: UtxoStorage>(
        &self,
        block: &crate::block::Block,
        utxo_storage: &mut S,
    ) -> Result<(), S::StorageError> {
        for tx in &block.data.transactions {
            match tx.data.kind {
                TransactionKind::Genesis => {
                    // Genesis transactions are handled during block creation.
                    // No UTXOs to consume; simply add outputs.
                    self.add_transaction_outputs(tx, utxo_storage).await?;
                }
                TransactionKind::Coinbase => {
                    // Coinbase transactions do not consume UTXOs; only add outputs.
                    self.add_transaction_outputs(tx, utxo_storage).await?;
                }
                TransactionKind::Payment => {
                    // Payment transactions consume UTXOs and add new ones.
                    self.process_payment_transaction(tx, utxo_storage).await?;
                }
            }
        }
        Ok(())
    }

    /// Adds the outputs of a transaction to the UTXO storage.
    async fn add_transaction_outputs<S: UtxoStorage>(
        &self,
        tx: &Transaction,
        utxo_storage: &mut S,
    ) -> Result<(), S::StorageError> {
        
        // Mapping outs to in Vec<(OutPoint, UTXO)> object
        let utxos: Vec<(OutPoint, UTXO)> = tx.data
            .outputs
            .iter()
            .enumerate()
            .map(|(vout, output)| {
                
                let out_point = OutPoint {
                    txid: tx.data.hash(),
                    vout: vout as u32, // safe cast
                };
                
                let utxo = UTXO {
                    txid: out_point.txid.clone(),
                    vout: out_point.vout,
                    value: output.value,
                    owner: output.recipient.clone(),
                };
                
                (out_point, utxo) 
            }).collect();
        
        
        // Call batch_put_utxos
        utxo_storage.batch_put_utxos(utxos).await
    }

    /// Processes a Payment transaction by consuming inputs and adding outputs.
    async fn process_payment_transaction<S: UtxoStorage>(
        &self,
        tx: &Transaction,
        utxo_storage: &mut S,
    ) -> Result<(), S::StorageError> {
        // Remove consumed UTXOs via batch
        
        
        // Convert TransactionIn's into Vec<OutPoint>
        let txins_out_points = tx.data.inputs
            .iter()
            .map(|txin| txin.previous_output.clone())
            .collect();
        
        
        // call batch_remove_utxos
        utxo_storage.batch_remove_utxos(txins_out_points).await?;
        
        
        // Add new UTXOs from transaction outputs
        self.add_transaction_outputs(tx, utxo_storage).await
    }
}
