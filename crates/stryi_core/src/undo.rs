use std::collections::HashSet;
use serde::{Deserialize, Serialize};
use crate::block::Block;
use crate::error::StryiCoreError;
use crate::transactions::{OutPoint, UTXO, Transaction};

/// BlockUndo stores the aggregated state changes made by a block.
/// It contains a set of spent UTXOs (with full details) and a set of created outpoints.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct BlockUndo {
    pub spent_utxos: HashSet<(OutPoint, UTXO)>,
    pub created_outpoints: HashSet<OutPoint>,
}

impl Block {
    /// Creates a BlockUndo record from the block.
    ///
    /// It iterates over each transaction and, for each input, uses the provided lookup
    /// function to retrieve the UTXO from the pre-block state. For each output, it computes
    /// an OutPoint using the transaction hash and the output index.
    ///
    /// The lookup function must be async and return the full UTXO data corresponding to the given OutPoint.
    pub async fn create_undo<F, Fut>(&self, utxo_lookup: F) -> Result<BlockUndo, StryiCoreError>
    where
        F: Fn(&OutPoint) -> Fut,
        Fut: Future<Output = Option<UTXO>>,
    {
        let mut spent_utxos = HashSet::new();
        let mut created_outpoints = HashSet::new();

        for tx in &self.data.transactions {
            process_inputs(tx, &utxo_lookup, &mut spent_utxos).await?;
            process_outputs(tx, &mut created_outpoints);
        }

        Ok(BlockUndo {
            spent_utxos,
            created_outpoints,
        })
    }
}

/// Processes the inputs of a transaction, inserting (OutPoint, UTXO) pairs into `spent`.
/// Uses async lookup function to retrieve UTXO data.
async fn process_inputs<F, Fut>(
    tx: &Transaction,
    utxo_lookup: &F,
    spent: &mut HashSet<(OutPoint, UTXO)>,
) -> Result<(), StryiCoreError>
where
    F: Fn(&OutPoint) -> Fut,
    Fut: Future<Output = Option<UTXO>>,
{
    for input in &tx.data.inputs {
        let utxo = utxo_lookup(&input.previous_output)
            .await
            .ok_or(StryiCoreError::TxMissingUtxo {
                txid: input.previous_output.txid,
                vout: input.previous_output.vout,
            })?;

        spent.insert((input.previous_output, utxo));
    }
    Ok(())
}

/// Processes the outputs of a transaction, computing OutPoints for each output.
fn process_outputs(tx: &Transaction, created: &mut HashSet<OutPoint>) {
    let txid = tx.data.hash();
    for (index, _output) in tx.data.outputs.iter().enumerate() {
        let outpoint = OutPoint {
            txid,
            vout: index as u32,
        };
        created.insert(outpoint);
    }
}

#[cfg(test)]
mod tests {
    use crate::address::AccountAddress;
    use crate::block::{BlockData, BlockHash, BlockHeader};
    use crate::merkletree::MerkleHash;
    use crate::transactions::{TransactionData, TransactionHash, TransactionIn, TransactionKind, TransactionOut};
    use super::*;

    #[tokio::test]
    async fn test_create_undo_success() {
        // Create test data
        let dummy_txid = TransactionHash::new(&[1u8; 32]);
        let dummy_outpoint = OutPoint {
            txid: dummy_txid,
            vout: 0,
        };
        let dummy_utxo = UTXO {
            txid: dummy_txid,
            vout: 0,
            value: 1000,
            owner: AccountAddress::new(&[2u8; 20]),
        };

        // Create transaction data
        let block = create_test_block(&dummy_outpoint);

        // Define async lookup closure - now using clone inside async block
        let lookup = |op: &OutPoint| {
            let op = *op;  // Clone the input parameter
            async move {
                if op == dummy_outpoint {
                    Some(dummy_utxo)
                } else {
                    None
                }
            }
        };
        
        // Call create_undo with await
        let undo = block.create_undo(lookup)
            .await
            .expect("Undo creation should succeed");

        // Verify spent UTXOs
        assert!(
            undo.spent_utxos.contains(&(dummy_outpoint, dummy_utxo)),
            "Spent UTXO should be recorded"
        );

        // Verify created outpoints
        let txid = block.data.transactions[0].data.hash();
        let expected_outpoint0 = OutPoint {
            txid,
            vout: 0,
        };
        let expected_outpoint1 = OutPoint {
            txid: txid,
            vout: 1,
        };

        assert!(
            undo.created_outpoints.contains(&expected_outpoint0),
            "First created outpoint should be present"
        );
        assert!(
            undo.created_outpoints.contains(&expected_outpoint1),
            "Second created outpoint should be present"
        );

        assert_eq!(undo.spent_utxos.len(), 1, "There should be exactly one spent UTXO recorded");
        assert_eq!(undo.created_outpoints.len(), 2, "There should be exactly two created outpoints");
    }

    #[tokio::test]
    async fn test_create_undo_missing_utxo() {
        let dummy_txid = TransactionHash::new(&[5u8; 32]);
        let missing_outpoint = OutPoint {
            txid: dummy_txid,
            vout: 0,
        };

        let block = create_test_block(&missing_outpoint);

        // Define async lookup that always returns None - no closure capture issues here
        let lookup = |_op: &OutPoint| async { None };

        // Call create_undo and expect an error
        let result = block.create_undo(lookup).await;
        assert!(result.is_err(), "Expected error for missing UTXO");

        if let Err(e) = result {
            match e {
                StryiCoreError::TxMissingUtxo { txid, vout } => {
                    assert_eq!(txid, dummy_txid);
                    assert_eq!(vout, 0);
                }
                _ => panic!("Expected TxMissingUtxo error, got {:?}", e),
            }
        }
    }

    // Helper function to create test block
    fn create_test_block(input_outpoint: &OutPoint) -> Block {
        let tx_in = TransactionIn {
            previous_output: *input_outpoint,
            sequence: 0,
        };

        let tx_out1 = TransactionOut {
            value: 600,
            recipient: AccountAddress::new(&[3u8; 20]),
        };
        let tx_out2 = TransactionOut {
            value: 400,
            recipient: AccountAddress::new(&[2u8; 20]),
        };

        let tx_data = TransactionData {
            version: 0,
            kind: TransactionKind::Payment,
            inputs: vec![tx_in],
            outputs: vec![tx_out1, tx_out2],
        };

        let transaction = Transaction {
            data: tx_data,
            signature: Default::default(),
        };

        Block {
            header: BlockHeader {
                version: 1,
                merkle_root_hash: MerkleHash::empty(),
                previous_block_hash: BlockHash::empty(),
                height: 0,
                difficulty_bits: 1,
                timestamp: 0,
                nonce: 0,
                is_genesis: false,
            },
            data: BlockData {
                transactions: vec![transaction],
            },
        }
    }
}