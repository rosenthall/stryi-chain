use crate::{
    block::{Block, BlockData},
    consensus::{ConsensusConsts, validator::tx},
    dependencies::DependencyGraph,
    error::{StorageLayer, StryiCoreError},
    storage::{StorageError, UtxoStorage},
    transactions::{OutPoint, TransactionKind, UTXO},
};
use dashmap::{DashMap, DashSet};
use rayon::prelude::*;
use std::{collections::HashSet, sync::Arc};

/// Performs all the cheap checks that do **not** touch the UTXO set.
pub fn validate_block_structure(block: &Block) -> Result<(), StryiCoreError> {
    ensure_unique_txs(block)?;
    ensure_transaction_composition(block)?;
    ensure_unique_inputs(&block.data)?;
    ensure_coinbase_context(block)?;

    Ok(())
}

fn ensure_coinbase_context(block: &Block) -> Result<(), StryiCoreError> {
    if block.header.is_genesis() {
        return Ok(());
    }

    let (height, parent) = match block.data.transactions.first().map(|tx| &tx.data.kind) {
        Some(TransactionKind::Coinbase { height, parent }) => (*height, *parent),
        // Missing or misplaced Coinbase is rejected by ensure_transaction_composition.
        _ => return Ok(()),
    };

    if height != block.header.height || parent != block.header.previous_block_hash {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "Coinbase height and parent must match the block header".into(),
        });
    }

    Ok(())
}

/// Rejects blocks with duplicated transactions.
fn ensure_unique_txs(block: &Block) -> Result<(), StryiCoreError> {
    let uniq: HashSet<_> = block.data.transactions.iter().cloned().collect();

    if uniq.len() != block.data.transactions.len() {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "Block contains duplicated transactions".into(),
        });
    }

    Ok(())
}

/// Non-genesis blocks must start with exactly one Coinbase, followed only by Payments.
fn ensure_transaction_composition(block: &Block) -> Result<(), StryiCoreError> {
    if block.header.is_genesis() {
        return Ok(());
    }

    let (first, rest) = match block.data.transactions.split_first() {
        Some(pair) => pair,
        None => {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Block transaction list cannot be empty".into(),
            });
        }
    };

    if first.data.kind == TransactionKind::Genesis {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "Genesis transaction is forbidden in non-genesis blocks".into(),
        });
    }

    if !matches!(first.data.kind, TransactionKind::Coinbase { .. }) {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "First transaction must be a Coinbase transaction".into(),
        });
    }

    for (offset, tx) in rest.iter().enumerate() {
        let idx = offset + 1;
        match tx.data.kind {
            TransactionKind::Payment => {}
            TransactionKind::Coinbase { .. } => {
                return Err(StryiCoreError::ConsensusValidationFailed {
                    details: format!(
                        "Block cannot contain multiple Coinbase transactions (extra Coinbase at index {idx})"
                    ),
                });
            }
            TransactionKind::Genesis => {
                return Err(StryiCoreError::ConsensusValidationFailed {
                    details: format!(
                        "Genesis transaction is forbidden in non-genesis blocks (found at index {idx})"
                    ),
                });
            }
        }
    }

    Ok(())
}

/// Fails if any input is used twice *inside* the same block.
fn ensure_unique_inputs(data: &BlockData) -> Result<(), StryiCoreError> {
    let mut seen = HashSet::<OutPoint>::new();
    for tx in &data.transactions {
        if tx.data.kind != TransactionKind::Payment {
            continue;
        }

        for inp in &tx.data.inputs {
            if !seen.insert(inp.previous_output) {
                return Err(StryiCoreError::TxDoubleSpend {
                    txid: inp.previous_output.txid,
                    vout: inp.previous_output.vout,
                });
            }
        }
    }

    Ok(())
}

/// Full in-block validation + reward rule.
///
/// 1. Build dependency graph & pull required UTXOs;  
/// 2. Run per-tx validation in **parallel execution groups**;  
/// 3. Ensure `coinbase <= subsidy + sum(fees)` (non-genesis).
pub fn validate_transactions<US: UtxoStorage + Send>(
    block: &Block,
    rules: &ConsensusConsts,
    utxo_storage: &US,
) -> Result<(), StryiCoreError> {
    let (graph, managed) = build_dependency_context(&block.data, utxo_storage)?;

    let in_block = Arc::new(DashMap::<OutPoint, UTXO>::new());
    let spent = Arc::new(DashSet::<OutPoint>::new());

    for grp in graph.get_parallel_execution_groups() {
        validate_group(&grp, &block.data, &managed, &in_block, &spent)?;
    }

    if !block.header.is_genesis() {
        reward_rule(block, rules, &managed, &in_block)?;
    }

    Ok(())
}

/// Builds a dependency graph and fetches external UTXOs in one go.
fn build_dependency_context<US: UtxoStorage>(
    data: &BlockData,
    utxo_storage: &US,
) -> Result<(DependencyGraph, Arc<DashMap<OutPoint, UTXO>>), StryiCoreError> {
    let mut graph = DependencyGraph::build(data)?;
    graph.validate_order(data)?;

    let external_deps = graph.get_external_dependencies();
    let managed_utxos = Arc::new(DashMap::<OutPoint, UTXO>::new());

    if !external_deps.is_empty() {
        let utxos_result = utxo_storage.batch_get_utxos(external_deps.iter().map(|u| u.to_owned()));

        match utxos_result {
            Ok(existing) => {
                for (op, utxo) in existing {
                    managed_utxos.insert(op, utxo);
                }
            }
            Err(StorageError::UtxoNotFound(outpoint)) => {
                return Err(StryiCoreError::TxMissingUtxo {
                    txid: outpoint.txid,
                    vout: outpoint.vout,
                });
            }
            Err(error) => return Err(StryiCoreError::storage(StorageLayer::Utxo, error)),
        }
    }

    Ok((graph, managed_utxos))
}

/// Validates all txs of the *parallel* execution group concurrently.
fn validate_group(
    idxs: &[usize],
    data: &BlockData,
    managed: &Arc<DashMap<OutPoint, UTXO>>,
    in_block: &Arc<DashMap<OutPoint, UTXO>>,
    spent: &Arc<DashSet<OutPoint>>,
) -> Result<(), StryiCoreError> {
    idxs.par_iter().try_for_each(|&index| {
        tx::validate_transaction(&data.transactions[index], managed, in_block, spent)
    })
}

/// Ensures `coinbase <= block_subsidy(height) + sum(fees)`.
fn reward_rule(
    block: &Block,
    rules: &ConsensusConsts,
    managed: &DashMap<OutPoint, UTXO>,
    in_block: &DashMap<OutPoint, UTXO>,
) -> Result<(), StryiCoreError> {
    let fees = tx::calculate_total_fees(block, managed, in_block)?;
    let subsidy = rules.block_subsidy(block.header.height);

    let max_reward =
        subsidy
            .checked_add(fees)
            .ok_or(StryiCoreError::ConsensusValidationFailed {
                details: "Overflow while computing (subsidy + fees)".into(),
            })?;

    let coinbase_val = block.data.transactions[0].data.outputs[0].value;
    if coinbase_val > max_reward {
        return Err(StryiCoreError::ConsensusInvalidCoinbaseAmount {
            max_expected: max_reward,
            actual: coinbase_val,
        });
    }

    Ok(())
}
