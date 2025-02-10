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
    /// The lookup function must return the full UTXO data corresponding to the given OutPoint.
    pub fn create_undo<F>(&self, mut utxo_lookup: F) -> Result<BlockUndo, StryiCoreError>
    where
        F: FnMut(&OutPoint) -> Option<UTXO>,
    {
        let mut spent_utxos = HashSet::new();
        let mut created_outpoints = HashSet::new();

        for tx in &self.data.transactions {
            process_inputs(tx, &mut utxo_lookup, &mut spent_utxos)?;
            process_outputs(tx, &mut created_outpoints);
        }

        Ok(BlockUndo {
            spent_utxos,
            created_outpoints,
        })
    }
}

/// Processes the inputs of a transaction, inserting (OutPoint, UTXO) pairs into `spent`.
fn process_inputs<F>(
    tx: &Transaction,
    utxo_lookup: &mut F,
    spent: &mut HashSet<(OutPoint, UTXO)>,
) -> Result<(), StryiCoreError>
where
    F: FnMut(&OutPoint) -> Option<UTXO>,
{
    for input in &tx.data.inputs {
        let utxo = utxo_lookup(&input.previous_output).ok_or_else(|| {
            StryiCoreError::TxMissingUtxo {
                txid: input.previous_output.txid.clone(),
                vout: input.previous_output.vout,
            }
        })?;
        spent.insert((input.previous_output.clone(), utxo));
    }
    Ok(())
}

/// Processes the outputs of a transaction, computing OutPoints for each output.
fn process_outputs(tx: &Transaction, created: &mut HashSet<OutPoint>) {
    let txid = tx.data.hash();
    for (index, _output) in tx.data.outputs.iter().enumerate() {
        let outpoint = OutPoint {
            txid: txid.clone(),
            vout: index as u32,
        };
        created.insert(outpoint);
    }
}


#[cfg(test)]
mod tests {
    use crate::address::AccountAddress;
    use crate::block::{Block, BlockData, BlockHeader, BlockHash};
    use crate::error::StryiCoreError;
    use crate::transactions::{Transaction, TransactionData, TransactionIn, TransactionOut, OutPoint, UTXO, TransactionHash, TransactionKind};

    #[test]
    fn test_create_undo_success() {
        // Create a dummy transaction outputs and utxos
        let dummy_txid = TransactionHash::new(&[1u8; 32]);
        let dummy_outpoint = OutPoint {
            txid: dummy_txid.clone(),
            vout: 0,
        };
        let dummy_utxo = UTXO {
            txid: dummy_txid.clone(),
            vout: 0,
            value: 1000,
            owner: AccountAddress::new(&[2u8; 20]),
        };

        // Create a transaction that spends the dummy_outpoint and creates two outputs.
        let tx_in = TransactionIn {
            previous_output: dummy_outpoint.clone(),
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

        // Create the transaction. Use Default::default() for the signature value.
        let transaction = Transaction {
            data: tx_data,
            signature: Default::default(),
        };

        let block_data = BlockData {
            transactions: vec![transaction],
        };

        let block_header = BlockHeader {
            version: 1,
            merkle_root_hash: [0u8; 32],
            previous_block_hash: BlockHash::empty(),
            height: 0,
            difficulty_bits: 1,
            timestamp: 0,
            nonce: 0,
            is_genesis: false,
        };

        let block = Block {
            header: block_header,
            data: block_data,
        };

        // Define a lookup closure that returns the dummy_utxo for the dummy_outpoint.
        let lookup = |op: &OutPoint| {
            if op == &dummy_outpoint {
                Some(dummy_utxo.clone())
            } else {
                None
            }
        };

        // Call create_undo.
        let undo = block.create_undo(lookup).expect("Undo creation should succeed");

        // Verify that spent_utxos contains the expected pair.
        assert!(
            undo.spent_utxos.contains(&(dummy_outpoint.clone(), dummy_utxo.clone())),
            "Spent UTXO should be recorded"
        );

        // The created_outpoints are computed from each transaction’s hash.
        // Here we call TransactionData::hash() on the transaction data to determine the txid.
        let txid = block.data.transactions[0].data.hash();
        let expected_outpoint0 = OutPoint {
            txid: txid.clone(),
            vout: 0,
        };
        let expected_outpoint1 = OutPoint {
            txid: txid.clone(),
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

        // Check that the sets have the expected sizes.
        assert_eq!(undo.spent_utxos.len(), 1, "There should be exactly one spent UTXO recorded");
        assert_eq!(undo.created_outpoints.len(), 2, "There should be exactly two created outpoints");
    }

    #[test]
    fn test_create_undo_missing_utxo() {
        // Create a dummy transaction input referencing an outpoint that is missing.
        let dummy_txid = TransactionHash::new(&[5u8; 32]);
        let missing_outpoint = OutPoint {
            txid: dummy_txid.clone(),
            vout: 0,
        };

        let tx_in = TransactionIn {
            previous_output: missing_outpoint.clone(),
            sequence: 0,
        };

        let tx_out = TransactionOut {
            value: 1000,
            recipient: AccountAddress::new(&[6u8; 20]),
        };

        let tx_data = TransactionData {
            version: 0,
            kind: TransactionKind::Payment,
            inputs: vec![tx_in],
            outputs: vec![tx_out],
        };

        let transaction = Transaction {
            data: tx_data,
            signature: Default::default(),
        };

        let block_data = BlockData {
            transactions: vec![transaction],
        };

        let block_header = BlockHeader {
            version: 1,
            merkle_root_hash: [0u8; 32],
            previous_block_hash: BlockHash::empty(),
            height: 0,
            difficulty_bits: 1,
            timestamp: 0,
            nonce: 0,
            is_genesis: false,
        };

        let block = Block {
            header: block_header,
            data: block_data,
        };

        // Define a lookup function that always returns None.
        let lookup = |_op: &OutPoint| -> Option<UTXO> { None };

        // Call create_undo and expect an error.
        let result = block.create_undo(lookup);
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
}
