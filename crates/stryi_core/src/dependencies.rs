use crate::block::BlockData;
use crate::error::StryiCoreError;
use crate::transactions::OutPoint;
use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet};

/// DependencyGraph provides transaction validation and dependency analysis capabilities.
/// It combines efficient hash-based validation with graph-based analytics to enable
/// both fast validation and advanced transaction flow analysis.
#[derive(Debug)]
pub struct DependencyGraph {
    // Core validation structures
    pub(crate) output_index: HashMap<OutPoint, usize>, // Maps transaction outputs to their position in block
    spent_outputs: HashSet<OutPoint>,                  // Tracks which outputs have been spent
    external_inputs: HashSet<OutPoint>, // Stores UTXOs from previous blocks that need verification

    // Graph structures for advanced analysis
    graph: DiGraph<usize, ()>, // Directed graph representing transaction dependencies
    node_indices: Vec<NodeIndex>, // Maps transaction indices to graph nodes
}

impl DependencyGraph {
    /// Builds a dependency graph from block data, performing initial validation.
    /// Creates both hash-based validation structures and graph representation.
    ///
    /// Returns error if invalid dependencies are detected, such as duplicate outputs.
    pub fn build(block_data: &BlockData) -> Result<Self, StryiCoreError> {
        let graph_struct = DiGraph::new();
        let node_indices = Vec::new();

        let mut dep_graph = Self {
            output_index: HashMap::new(),
            spent_outputs: HashSet::new(),
            external_inputs: HashSet::new(),
            graph: graph_struct,
            node_indices,
        };

        // Initialize graph nodes for all transactions
        for i in 0..block_data.transactions.len() {
            dep_graph.node_indices.push(dep_graph.graph.add_node(i));
        }

        // Index transaction outputs and detect duplicates
        for (tx_idx, tx) in block_data.transactions.iter().enumerate() {
            for (vout, _) in tx.data.outputs.iter().enumerate() {
                let outpoint = OutPoint {
                    txid: tx.data.hash(),
                    vout: vout as u32,
                };

                if dep_graph.output_index.insert(outpoint, tx_idx).is_some() {
                    return Err(StryiCoreError::TransactionDependencyError {
                        msg: "Duplicate output detected".to_string(),
                    });
                }
            }
        }

        // Build graph edges and identify external dependencies
        for (tx_idx, tx) in block_data.transactions.iter().enumerate() {
            for input in &tx.data.inputs {
                if let Some(&dep_tx_idx) = dep_graph.output_index.get(&input.previous_output) {
                    // Add edge for internal dependency
                    dep_graph.graph.add_edge(
                        dep_graph.node_indices[dep_tx_idx],
                        dep_graph.node_indices[tx_idx],
                        (),
                    );
                } else {
                    // Track external UTXO dependency
                    dep_graph.external_inputs.insert(input.previous_output);
                }
            }
        }

        Ok(dep_graph)
    }

    /// Validates the ordering of transactions within the block and prevents double spends.
    /// Uses efficient hash-based structures for quick validation.
    pub fn validate_order(&mut self, block_data: &BlockData) -> Result<(), StryiCoreError> {
        for (tx_idx, tx) in block_data.transactions.iter().enumerate() {
            for input in &tx.data.inputs {
                // Prevent double spending of outputs
                if !self.spent_outputs.insert(input.previous_output) {
                    return Err(StryiCoreError::TransactionDependencyError {
                        msg: "Double spend detected".to_string(),
                    });
                }

                // Ensure correct transaction ordering
                if let Some(&dep_tx_idx) = self.output_index.get(&input.previous_output) {
                    if dep_tx_idx >= tx_idx {
                        return Err(StryiCoreError::TransactionDependencyError {
                            msg: "Invalid transaction order".to_string(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Returns set of UTXOs from previous blocks that need verification
    pub fn get_external_dependencies(&self) -> &HashSet<OutPoint> {
        &self.external_inputs
    }

    /// Returns list of transactions that the given transaction depends on
    pub fn get_transaction_dependencies(&self, tx_idx: usize) -> Vec<usize> {
        if tx_idx >= self.node_indices.len() {
            return Vec::new();
        }

        self.graph
            .edges_directed(self.node_indices[tx_idx], Direction::Incoming)
            .map(|edge| self.graph[edge.source()])
            .collect()
    }

    /// Returns list of transactions that depend on the given transaction
    pub fn get_dependent_transactions(&self, tx_idx: usize) -> Vec<usize> {
        if tx_idx >= self.node_indices.len() {
            return Vec::new();
        }

        self.graph
            .edges_directed(self.node_indices[tx_idx], Direction::Outgoing)
            .map(|edge| self.graph[edge.target()])
            .collect()
    }

    /// Identifies groups of transactions that can be executed in parallel.
    /// Returns a vector of transaction groups, where transactions within each group
    /// have no dependencies on each other.
    pub fn get_parallel_execution_groups(&self) -> Vec<Vec<usize>> {
        let mut result = Vec::new();
        let mut processed = HashSet::new();

        while processed.len() < self.node_indices.len() {
            let mut current_group = Vec::new();

            // Find all transactions whose dependencies are satisfied
            for idx in 0..self.node_indices.len() {
                if processed.contains(&idx) {
                    continue;
                }

                let deps = self.get_transaction_dependencies(idx);
                if deps.iter().all(|dep_idx| processed.contains(dep_idx)) {
                    current_group.push(idx);
                }
            }

            // Mark current group as processed
            for idx in &current_group {
                processed.insert(*idx);
            }

            if !current_group.is_empty() {
                result.push(current_group);
            }
        }

        result
    }
}
