//! Mempool module that stores unconfirmed transactions, manages dependencies, 
//! and provides features such as Replace-by-Fee (RBF), topological ordering, 
//! and ancestor scoring for transaction selection.

mod fee_policy;
pub use fee_policy::*;

mod error;
mod types;
pub use error::*;

use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::prelude::{Direction, EdgeRef};
use tokio::sync::RwLock;

use crate::transactions::{Transaction, TransactionHash, OutPoint, UTXO};
use crate::mempool::types::{MemPoolConfig, MemPoolSyncData, MemPoolTx};

/// A type alias for the asynchronous UTXO lookup function.
/// It must return Some(UTXO) if the given outpoint is valid and unspent on-chain,
/// or None otherwise.
pub type UtxoLookup = Box<dyn Fn(&OutPoint) -> Pin<Box<dyn Future<Output = Option<UTXO>> + Send>> + Send + Sync>;

/// Primary mempool structure that holds a shared state of unconfirmed transactions,
/// along with a user-provided UTXO lookup function.
pub struct MemPool {
    /// Shared state, protected by RwLock.
    state: Arc<RwLock<MemPoolState>>,
    /// Function to asynchronously query the on-chain UTXOs.
    utxo_lookup: UtxoLookup,
}

/// Internal state of the mempool, including all transactions, indexes, and dependency graph.
struct MemPoolState {
    /// Maps transaction hash -> mempool entry (which includes fee, timestamp, etc.).
    transactions: HashMap<TransactionHash, MemPoolTx>,
    /// Maps each OutPoint -> transaction hash of the mempool transaction that spends it.
    outpoint_index: HashMap<OutPoint, TransactionHash>,
    /// Maps transaction hash -> NodeIndex in the dependency graph.
    hash_to_index: HashMap<TransactionHash, NodeIndex>,
    /// A directed acyclic graph (DAG) tracking dependencies (parent -> child).
    dependency_graph: DiGraph<TransactionHash, ()>,
    /// Fee calculator (configurable via FeePolicy).
    fee_calculator: FeeCalculator,
    /// Maximum number of transactions allowed in the mempool.
    max_size: usize,
    /// Time in seconds after which transactions are considered expired.
    expiry_time: u64,
}

impl MemPool {
    /// Returns the current Unix timestamp in seconds.
    fn current_timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Clock is set before Unix epoch")
            .as_secs()
    }

    /// Creates a new mempool instance with the specified configuration and UTXO lookup function.
    /// `config` controls parameters such as max_size, fee policy, expiry_time.
    /// `utxo_lookup` is used to verify whether inputs exist on-chain.
    pub fn new(config: MemPoolConfig, utxo_lookup: UtxoLookup) -> Self {
        Self {
            state: Arc::new(RwLock::new(MemPoolState {
                transactions: HashMap::with_capacity(config.max_size),
                outpoint_index: HashMap::with_capacity(config.max_size),
                hash_to_index: HashMap::with_capacity(config.max_size),
                dependency_graph: DiGraph::new(),
                fee_calculator: FeeCalculator::new(config.fee_policy),
                max_size: config.max_size,
                expiry_time: config.expiry_time,
            })),
            utxo_lookup,
        }
    }

    /// Serializes the mempool into a MemPoolSyncData structure for synchronization or persistence.
    /// Returns the encoded bytes or a Storage error on failure.
    pub async fn handle_get_state(&self) -> Result<Vec<u8>, MemPoolError> {
        let state = self.state.read().await;
        let sync_data = MemPoolSyncData {
            transactions: state
                .transactions
                .values()
                .map(|e| e.transaction.clone())
                .collect(),
            timestamp: Self::current_timestamp(),
        };
        bincode::serde::encode_to_vec(&sync_data, bincode::config::standard())
            .map_err(|e| MemPoolError::Storage(Box::new(e)))
    }

    /// Checks basic conditions: whether there's capacity for a new transaction,
    /// and whether this transaction is already in the mempool.
    async fn validate_basic(&self, state: &MemPoolState, tx_hash: &TransactionHash) -> Result<(), MemPoolError> {
        if state.transactions.len() >= state.max_size {
            return Err(MemPoolError::PoolFull { size: state.max_size });
        }
        if state.transactions.contains_key(tx_hash) {
            return Err(MemPoolError::DuplicateTransaction { hash: tx_hash.clone() });
        }
        Ok(())
    }

    /// Validates the transaction's inputs. Checks for missing UTXOs (in chain),
    /// and handles Replace-by-Fee if any outpoint is already spent by a lower-fee mempool transaction.
    /// If required_fee is not strictly higher than the conflict's fee, returns InsufficientFee error.
    async fn validate_inputs(
        &self,
        state: &mut MemPoolState,
        tx: &Transaction,
        required_fee: u64,
    ) -> Result<(), MemPoolError> {
        let mut missing = Vec::new();
        let mut conflicts = HashSet::new();

        for input in &tx.data.inputs {
            match state.outpoint_index.get(&input.previous_output) {
                Some(hash_in_mempool) => {
                    let conflict_entry = state
                        .transactions
                        .get(hash_in_mempool)
                        .expect("Inconsistent mempool indexing");
                    if required_fee <= conflict_entry.fee {
                        return Err(MemPoolError::InsufficientFee {
                            required: conflict_entry.fee + 1,
                            actual: required_fee,
                        });
                    }
                    conflicts.insert(hash_in_mempool.clone());
                }
                None => {
                    if (self.utxo_lookup)(&input.previous_output).await.is_none() {
                        missing.push(input.previous_output.clone());
                    }
                }
            }
        }

        if !missing.is_empty() {
            return Err(MemPoolError::MissingUtxos(missing));
        }

        for old_hash in conflicts {
            self.remove_with_descendants(state, &old_hash);
        }

        Ok(())
    }

    /// Inserts a new transaction into all relevant data structures:
    /// - Adds it as a node in the DAG
    /// - Marks all of its inputs and outputs in outpoint_index
    /// - Inserts the MemPoolTx into transactions map
    fn insert_transaction(state: &mut MemPoolState, tx: Transaction, tx_hash: TransactionHash, fee: u64) {
        let node_idx = state.dependency_graph.add_node(tx_hash.clone());
        state.hash_to_index.insert(tx_hash.clone(), node_idx);

        for input in &tx.data.inputs {
            state.outpoint_index.insert(input.previous_output.clone(), tx_hash.clone());
            if let Some(producer_hash) = state.outpoint_index.get(&input.previous_output) {
                if let Some(&producer_idx) = state.hash_to_index.get(producer_hash) {
                    state.dependency_graph.add_edge(producer_idx, node_idx, ());
                }
            }
        }
        for (vout_idx, _) in tx.data.outputs.iter().enumerate() {
            let outp = OutPoint {
                txid: tx_hash.clone(),
                vout: vout_idx as u32,
            };
            state.outpoint_index.insert(outp, tx_hash.clone());
        }

        let entry = MemPoolTx {
            transaction: tx,
            timestamp: Self::current_timestamp(),
            fee,
        };
        state.transactions.insert(tx_hash, entry);
    }

    /// Adds a transaction to the mempool. Checks capacity, duplicates, does input validation,
    /// handles RBF, and inserts the transaction if all checks pass.
    pub async fn add_transaction(&self, tx: Transaction) -> Result<(), MemPoolError> {
        let tx_hash = tx.data.hash();
        let needed_fee = {
            let read_state = self.state.read().await;
            read_state.fee_calculator.calculate_fee(&tx)
        };

        let mut state = self.state.write().await;
        self.validate_basic(&state, &tx_hash).await?;
        self.validate_inputs(&mut state, &tx, needed_fee).await?;
        Self::insert_transaction(&mut state, tx, tx_hash, needed_fee);
        Ok(())
    }

    /// Removes a transaction from the mempool along with all of its descendants in the DAG.
    /// A descendant is any transaction that depends on this transaction's outputs, directly or indirectly.
    fn remove_with_descendants(&self, state: &mut MemPoolState, root_hash: &TransactionHash) {
        let Some(root_idx) = state.hash_to_index.remove(root_hash) else {
            return;
        };

        let mut to_remove = HashSet::new();
        to_remove.insert(root_idx);
        let mut queue = VecDeque::new();
        queue.push_back(root_idx);

        while let Some(curr) = queue.pop_front() {
            let edges = state
                .dependency_graph
                .edges_directed(curr, Direction::Outgoing)
                .map(|e| e.target())
                .collect::<Vec<_>>();
            for child_idx in edges {
                if !to_remove.contains(&child_idx) {
                    to_remove.insert(child_idx);
                    queue.push_back(child_idx);
                }
            }
        }

        let mut removal_list: Vec<NodeIndex> = to_remove.into_iter().collect();
        removal_list.sort_by_key(|ni| ni.index());
        removal_list.reverse();

        for idx in &removal_list {
            if let Some(tx_hash_ref) = state.dependency_graph.node_weight(*idx) {
                let clone_hash = tx_hash_ref.clone();
                if let Some(old_tx) = state.transactions.remove(&clone_hash) {
                    for input in &old_tx.transaction.data.inputs {
                        state.outpoint_index.remove(&input.previous_output);
                    }
                    for (vout_idx, _) in old_tx.transaction.data.outputs.iter().enumerate() {
                        let outp = OutPoint {
                            txid: clone_hash.clone(),
                            vout: vout_idx as u32,
                        };
                        state.outpoint_index.remove(&outp);
                    }
                }
                state.hash_to_index.remove(&clone_hash);
            }
        }

        for idx in removal_list {
            state.dependency_graph.remove_node(idx);
        }
    }

    /// Removes a transaction by its hash, using remove_with_descendants to ensure no invalid
    /// child transactions remain in the mempool.
    pub async fn remove_transaction(&self, tx_hash: TransactionHash) -> Result<(), MemPoolError> {
        let mut state = self.state.write().await;
        self.remove_with_descendants(&mut state, &tx_hash);
        Ok(())
    }

    /// Returns a set of transactions in a feasible order (parents before children),
    /// based on a topological approach plus ancestor scoring. This tries to group
    /// dependent transactions and pick the chain with the best effective fee rate.
    pub async fn get_best_transactions(&self, limit: usize) -> Result<Vec<Transaction>, MemPoolError> {
        let state = self.state.read().await;
        let ordered = topological_order(&state.dependency_graph, &state.hash_to_index);
        let scored = build_ancestor_scores(&ordered, &state);
        let mut all_nodes: Vec<_> = scored.into_iter().collect();
        all_nodes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut result = Vec::new();
        let mut used_nodes = HashSet::new();

        for (node_idx, _) in all_nodes {
            if used_nodes.contains(&node_idx) {
                continue;
            }
            if let Some(tx_hash) = state.dependency_graph.node_weight(node_idx) {
                if let Some(memtx) = state.transactions.get(tx_hash) {
                    let ancestors = gather_ancestors(node_idx, &state.dependency_graph);
                    let mut can_select = true;
                    for anc_idx in &ancestors {
                        if used_nodes.contains(anc_idx) {
                            can_select = false;
                            break;
                        }
                    }
                    if can_select {
                        for anc_idx in &ancestors {
                            used_nodes.insert(*anc_idx);
                            if let Some(dep_hash) = state.dependency_graph.node_weight(*anc_idx) {
                                if let Some(dep_tx) = state.transactions.get(dep_hash) {
                                    result.push(dep_tx.transaction.clone());
                                    if result.len() >= limit {
                                        return Ok(result);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if result.len() >= limit {
                break;
            }
        }
        Ok(result)
    }

    /// After a new block is confirmed, remove the included transactions from the mempool
    /// along with any descendants that depended on them.
    pub async fn update_on_block(&self, block: crate::block::BlockData) -> Result<(), MemPoolError> {
        let mut state = self.state.write().await;
        for tx in &block.transactions {
            let tx_hash = tx.data.hash();
            self.remove_with_descendants(&mut state, &tx_hash);
        }
        Ok(())
    }

    /// Removes any transactions whose timestamps exceed the configured expiry_time,
    /// cleaning out stale entries.
    pub async fn cleanup_expired(&self) -> Result<(), MemPoolError> {
        let now = Self::current_timestamp();
        let mut state = self.state.write().await;

        let expired: Vec<_> = state
            .transactions
            .iter()
            .filter_map(|(h, memtx)| {
                if now.saturating_sub(memtx.timestamp) > state.expiry_time {
                    Some(h.clone())
                } else {
                    None
                }
            })
            .collect();

        for tx_hash in expired {
            self.remove_with_descendants(&mut state, &tx_hash);
        }
        Ok(())
    }

    /// Restores the mempool from a serialized snapshot. Clears the existing data
    /// and populates it with the given transactions, recalculating fees and indexes.
    pub async fn restore_state(&self, data: Vec<u8>) -> Result<(), MemPoolError> {
        let sync_data: MemPoolSyncData =
            bincode::serde::decode_from_slice(&data, bincode::config::standard())
                .map_err(|e| MemPoolError::Storage(Box::new(e)))?
                .0;

        let mut state = self.state.write().await;

        state.transactions.clear();
        state.outpoint_index.clear();
        state.dependency_graph = DiGraph::new();
        state.hash_to_index.clear();

        for tx in sync_data.transactions {
            let tx_hash = tx.data.hash();
            let fee = state.fee_calculator.calculate_fee(&tx);
            let entry = MemPoolTx {
                transaction: tx.clone(),
                timestamp: Self::current_timestamp(),
                fee,
            };
            state.transactions.insert(tx_hash.clone(), entry);
            let node_idx = state.dependency_graph.add_node(tx_hash.clone());
            state.hash_to_index.insert(tx_hash.clone(), node_idx);

            for inp in &tx.data.inputs {
                state.outpoint_index.insert(inp.previous_output.clone(), tx_hash.clone());
            }
            for (vout_idx, _) in tx.data.outputs.iter().enumerate() {
                let outp = OutPoint {
                    txid: tx_hash.clone(),
                    vout: vout_idx as u32,
                };
                state.outpoint_index.insert(outp, tx_hash.clone());
            }
        }
        Ok(())
    }
}

/// Performs a topological sort of all nodes in the DAG, returning
/// a list of NodeIndexes in topological order (parents before children).
fn topological_order(
    graph: &DiGraph<TransactionHash, ()>,
    hash_index: &HashMap<TransactionHash, NodeIndex>
) -> Vec<NodeIndex> {
    let mut visited = HashSet::new();
    let mut stack = Vec::new();
    let mut order = Vec::new();

    for idx in hash_index.values() {
        if !visited.contains(idx) {
            dfs_topo(*idx, graph, &mut visited, &mut stack);
        }
    }
    while let Some(node) = stack.pop() {
        order.push(node);
    }
    order
}

/// Recursive DFS helper for topological sorting. Traverses all descendants
/// and pushes the node onto the stack when finished.
fn dfs_topo(
    current: NodeIndex,
    graph: &DiGraph<TransactionHash, ()>,
    visited: &mut HashSet<NodeIndex>,
    stack: &mut Vec<NodeIndex>
) {
    visited.insert(current);
    let edges = graph
        .edges_directed(current, Direction::Outgoing)
        .map(|e| e.target())
        .collect::<Vec<_>>();
    for nxt in edges {
        if !visited.contains(&nxt) {
            dfs_topo(nxt, graph, visited, stack);
        }
    }
    stack.push(current);
}

/// Gathers all ancestors (including the node itself) by following incoming edges upward.
/// The result is a list of NodeIndexes representing this node and its transitive parents.
fn gather_ancestors(
    start: NodeIndex,
    graph: &DiGraph<TransactionHash, ()>
) -> Vec<NodeIndex> {
    let mut result = Vec::new();
    let mut stack = vec![start];
    let mut visited = HashSet::new();

    while let Some(n) = stack.pop() {
        if !visited.insert(n) {
            continue;
        }
        result.push(n);
        let incoming = graph
            .edges_directed(n, Direction::Incoming)
            .map(|e| e.source())
            .collect::<Vec<_>>();
        for src in incoming {
            if !visited.contains(&src) {
                stack.push(src);
            }
        }
    }
    result.reverse();
    result
}

/// Computes a rough "ancestor-based" fee rate for each node:
/// 1) Collect all ancestors (including the node).
/// 2) Sum total fees, sum total serialized sizes.
/// 3) Rate = total_fee / total_size.
/// Returns a vector of (NodeIndex, rate).
fn build_ancestor_scores(
    order: &[NodeIndex],
    state: &MemPoolState
) -> Vec<(NodeIndex, f64)> {
    let mut scores = Vec::new();

    for &idx in order {
        let ancestors = gather_ancestors(idx, &state.dependency_graph);
        let mut total_fee = 0u64;
        let mut total_size = 0usize;

        for anc in ancestors {
            if let Some(tx_hash) = state.dependency_graph.node_weight(anc) {
                if let Some(mem_tx) = state.transactions.get(tx_hash) {
                    total_fee = total_fee.saturating_add(mem_tx.fee);
                    let bytes = bincode::serde::encode_to_vec(&mem_tx.transaction, bincode::config::standard())
                        .map(|v| v.len())
                        .unwrap_or(0);
                    total_size = total_size.saturating_add(bytes);
                }
            }
        }

        let rate = if total_size == 0 {
            0.0
        } else {
            total_fee as f64 / total_size as f64
        };
        scores.push((idx, rate));
    }
    scores
}
