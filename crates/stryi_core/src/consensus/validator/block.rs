use crate::{
    block::{Block, BlockData},
    consensus::{ConsensusConsts, validator::tx},
    dependencies::DependencyGraph,
    error::StryiCoreError,
    storage::UtxoStorage,
    transactions::{OutPoint, TransactionKind, UTXO},
};
use dashmap::{DashMap, DashSet};
use std::{collections::HashSet, sync::Arc};
use tokio::task::JoinSet;

/// Performs all the cheap checks that do **not** touch the UTXO set.
pub fn validate_block_structure(block: &Block) -> Result<(), StryiCoreError> {
    ensure_unique_txs(block)?;
    ensure_coinbase_first(block)?;
    ensure_unique_inputs(&block.data)?;
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

/// Ensures the very first tx is Coinbase (except for genesis).
fn ensure_coinbase_first(block: &Block) -> Result<(), StryiCoreError> {
    if block.header.is_genesis() {
        return Ok(());
    }
    match block.data.transactions.first() {
        Some(tx) if tx.data.kind == TransactionKind::Coinbase => Ok(()),
        _ => Err(StryiCoreError::ConsensusValidationFailed {
            details: "First transaction must be a Coinbase transaction".into(),
        }),
    }
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
pub async fn validate_transactions<US: UtxoStorage + Send>(
    block: &Block,
    rules: &ConsensusConsts,
    utxo_storage: &US,
) -> Result<(), StryiCoreError> {
    let (graph, managed) = build_dependency_context(&block.data, utxo_storage).await?;

    let in_block = Arc::new(DashMap::<OutPoint, UTXO>::new());
    let spent = Arc::new(DashSet::<OutPoint>::new());

    for grp in graph.get_parallel_execution_groups() {
        validate_group(&grp, &block.data, &managed, &in_block, &spent).await?;
    }

    if !block.header.is_genesis() {
        reward_rule(block, rules, &managed, &in_block)?;
    }
    Ok(())
}

/// Builds a dependency graph and fetches external UTXOs in one go.
async fn build_dependency_context<US: UtxoStorage>(
    data: &BlockData,
    utxo_storage: &US,
) -> Result<(DependencyGraph, Arc<DashMap<OutPoint, UTXO>>), StryiCoreError> {
    let mut graph = DependencyGraph::build(data)?;
    graph.validate_order(data)?;

    let external_deps = graph.get_external_dependencies();
    let managed_utxos = Arc::new(DashMap::<OutPoint, UTXO>::new());

    if !external_deps.is_empty() {
        let utxos_result = utxo_storage
            .batch_get_utxos(external_deps.iter().map(|u| u.to_owned()))
            .await;

        match utxos_result {
            Ok(existing) => {
                for (op, utxo) in existing {
                    managed_utxos.insert(op, utxo);
                }
            }
            Err(_) => {
                // pick any missing out-point to report
                if let Some(first_missing) = external_deps.iter().next() {
                    return Err(StryiCoreError::TxMissingUtxo {
                        txid: first_missing.txid,
                        vout: first_missing.vout,
                    });
                }
            }
        }
    }

    Ok((graph, managed_utxos))
}

/// Validates all txs of the *parallel* execution group concurrently.
async fn validate_group(
    idxs: &[usize],
    data: &BlockData,
    managed: &Arc<DashMap<OutPoint, UTXO>>,
    in_block: &Arc<DashMap<OutPoint, UTXO>>,
    spent: &Arc<DashSet<OutPoint>>,
) -> Result<(), StryiCoreError> {
    let mut set = JoinSet::new();

    for &i in idxs {
        let tx = data.transactions[i].clone();
        let managed = Arc::clone(managed);
        let in_block = Arc::clone(in_block);
        let spent = Arc::clone(spent);

        set.spawn(async move { tx::validate_transaction(&tx, &managed, &in_block, &spent).await });
    }

    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                set.abort_all();
                return Err(e);
            }
            Err(e) => {
                set.abort_all();
                return Err(StryiCoreError::ConsensusValidationFailed {
                    details: format!("Tokio join error: {e:?}"),
                });
            }
        }
    }
    Ok(())
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
