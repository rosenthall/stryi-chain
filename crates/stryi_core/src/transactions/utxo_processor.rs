use crate::error::StryiCoreError;
use crate::storage::UtxoStorage;
use crate::transactions::{Transaction, UTXO, OutPoint};

/// Applies a single transaction to the UTXO set:
/// - Removes inputs from the UTXO set (they are spent),
/// - Creates new UTXOs for the outputs.
pub async fn apply_transaction<S: UtxoStorage>(
    tx: &Transaction,
    utxo_storage: &mut S,
) -> Result<(), StryiCoreError> {
    let txid = tx.data.hash();

    // 1) Spend each input UTXO
    for input in &tx.data.inputs {
        utxo_storage
            .remove_utxo(&input.previous_output)
            .await
            .map_err(|_| {
                StryiCoreError::TxMissingUtxo {
                    txid: input.previous_output.txid,
                    vout: input.previous_output.vout,
                }
            })?;
    }

    // 2) Create new UTXOs from the outputs
    for (index, output) in tx.data.outputs.iter().enumerate() {
        let outpoint = OutPoint { txid, vout: index as u32 };
        let new_utxo = UTXO {
            txid,
            vout: index as u32,
            value: output.value,
            owner: output.recipient,
        };

        utxo_storage.put_utxo(&outpoint, new_utxo).await.map_err(|err| {
            StryiCoreError::Other {
                msg: format!("Failed to put UTXO: {err:?}"),
            }
        })?;
    }

    Ok(())
}

/// Applies all transactions in a block to the UTXO set, in order.
pub async fn apply_block<S: UtxoStorage>(
    block: &crate::block::Block,
    utxo_storage: &mut S,
) -> Result<(), StryiCoreError> {
    for tx in &block.data.transactions {
        apply_transaction(tx, utxo_storage).await?;
    }
    Ok(())
}
