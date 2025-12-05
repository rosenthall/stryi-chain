//! Per-transaction consensus rules.
//
//  validate_transaction – async routine used by block-level code
//  calculate_total_fees – helper to sum fees for the whole block

use crate::{
    address::AccountAddress,
    error::StryiCoreError,
    transactions::{OutPoint, Transaction, TransactionKind, UTXO},
};
use dashmap::{DashMap, DashSet};

/// Validates **one** transaction within the context of a block.
///
/// * `managed`   – UTXOs from previous blocks;  
/// * `in_block`  – UTXOs created earlier in this block;  
/// * `spent`     – thread-safe reservation set to close the double-spend window.
pub async fn validate_transaction(
    tx: &Transaction,
    managed: &DashMap<OutPoint, UTXO>,
    in_block: &DashMap<OutPoint, UTXO>,
    spent: &DashSet<OutPoint>,
) -> Result<(), StryiCoreError> {
    match tx.data.kind {
        TransactionKind::Genesis => validate_genesis_tx(tx),
        TransactionKind::Coinbase => validate_coinbase_tx(tx),
        TransactionKind::Payment => validate_payment_tx(tx, managed, in_block, spent).await,
    }
}

/// Genesis tx must have **no inputs** and **>=1 output**.
fn validate_genesis_tx(tx: &Transaction) -> Result<(), StryiCoreError> {
    if tx.data.inputs.is_empty() && !tx.data.outputs.is_empty() {
        Ok(())
    } else {
        Err(StryiCoreError::ConsensusValidationFailed {
            details: "Invalid Genesis transaction structure".into(),
        })
    }
}

/// Coinbase must have **no inputs** and **exactly one** output.
fn validate_coinbase_tx(tx: &Transaction) -> Result<(), StryiCoreError> {
    if tx.data.inputs.is_empty() && tx.data.outputs.len() == 1 {
        Ok(())
    } else {
        Err(StryiCoreError::ConsensusValidationFailed {
            details: "Invalid Coinbase transaction structure".into(),
        })
    }
}

/// Full payment-transaction validation.
///
/// Steps  
/// 1. Recover author key + verify signature;  
/// 2. Iterate inputs – reserve, check ownership, accumulate sum(inputs);  
/// 3. Accumulate sum(outputs);  
/// 4. Require sum(inputs) >= sum(outputs).
async fn validate_payment_tx(
    tx: &Transaction,
    managed: &DashMap<OutPoint, UTXO>,
    in_block: &DashMap<OutPoint, UTXO>,
    spent: &DashSet<OutPoint>,
) -> Result<(), StryiCoreError> {
    // Signature & author
    let pk = tx
        .recover_public_key()
        .map_err(|_| StryiCoreError::ConsensusValidationFailed {
            details: "Failed to recover public key".into(),
        })?;
    tx.verify_signature(&pk)
        .map_err(|_| StryiCoreError::ConsensusValidationFailed {
            details: "Invalid transaction signature".into(),
        })?;
    let author = AccountAddress::from_public_key(&pk);

    // Inputs
    let mut in_sum = 0u64;
    for inp in &tx.data.inputs {
        if !spent.insert(inp.previous_output) {
            return Err(StryiCoreError::TxDoubleSpend {
                txid: inp.previous_output.txid,
                vout: inp.previous_output.vout,
            });
        }

        let utxo = in_block
            .get(&inp.previous_output)
            .or_else(|| managed.get(&inp.previous_output))
            .ok_or(StryiCoreError::TxMissingUtxo {
                txid: inp.previous_output.txid,
                vout: inp.previous_output.vout,
            })?;

        if utxo.owner != author {
            return Err(StryiCoreError::TxWrongOwner {
                expected: utxo.owner,
                actual: author,
            });
        }

        in_sum =
            in_sum
                .checked_add(utxo.value)
                .ok_or(StryiCoreError::ConsensusValidationFailed {
                    details: "Overflow while summing inputs".into(),
                })?;
    }

    // Outputs
    let out_sum = tx
        .data
        .outputs
        .iter()
        .try_fold(0u64, |acc, o| acc.checked_add(o.value))
        .ok_or(StryiCoreError::ConsensusValidationFailed {
            details: "Overflow while summing outputs".into(),
        })?;

    if out_sum > in_sum {
        return Err(StryiCoreError::TxInsufficientInputValue {
            input_sum: in_sum,
            output_sum: out_sum,
        });
    }

    Ok(())
}

/// Computes **aggregate fee** (sum(inputs) − sum(outputs)) for all *payment* txs.
pub fn calculate_total_fees(
    block: &crate::block::Block,
    managed: &DashMap<OutPoint, UTXO>,
    in_block: &DashMap<OutPoint, UTXO>,
) -> Result<u64, StryiCoreError> {
    let mut total = 0u64;

    for tx in &block.data.transactions {
        if tx.data.kind != TransactionKind::Payment {
            continue;
        }

        // sum(inputs)
        let mut inputs = 0u64;
        for inp in &tx.data.inputs {
            let u = in_block
                .get(&inp.previous_output)
                .or_else(|| managed.get(&inp.previous_output))
                .ok_or(StryiCoreError::TxMissingUtxo {
                    txid: inp.previous_output.txid,
                    vout: inp.previous_output.vout,
                })?;
            inputs =
                inputs
                    .checked_add(u.value)
                    .ok_or(StryiCoreError::ConsensusValidationFailed {
                        details: "Overflow while summing input values".into(),
                    })?;
        }

        // sum(outputs)
        let outputs = tx
            .data
            .outputs
            .iter()
            .try_fold(0u64, |acc, o| acc.checked_add(o.value))
            .ok_or(StryiCoreError::ConsensusValidationFailed {
                details: "Overflow while summing output values".into(),
            })?;

        // fee >= 0
        let fee = inputs
            .checked_sub(outputs)
            .ok_or(StryiCoreError::ConsensusValidationFailed {
                details: "Outputs exceed inputs".into(),
            })?;

        total = total
            .checked_add(fee)
            .ok_or(StryiCoreError::ConsensusValidationFailed {
                details: "Overflow while accumulating total fees".into(),
            })?;
    }

    Ok(total)
}
