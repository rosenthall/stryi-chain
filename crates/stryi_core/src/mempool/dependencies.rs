//! DependencyTracker is responsible for maintaining a directed acyclic graph (DAG)
//! of mempool transactions, where edges indicate dependencies (parent -> child).

use crate::transactions::TransactionHash;
use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet, VecDeque};

/// DependencyTracker stores a DAG of mempool transactions and provides
/// methods to add, remove, and gather descendants or ancestors.
/// Each node is a TransactionHash, while edges represent that the child
/// transaction depends on outputs from the parent transaction.
pub struct DependencyTracker {
    /// The underlying directed graph of transaction hashes.
    pub(crate) graph: DiGraph<TransactionHash, ()>,
    /// A mapping from TransactionHash to the corresponding node index in the DAG.
    pub(crate) indices: HashMap<TransactionHash, NodeIndex>,
}

impl Default for DependencyTracker {
    fn default() -> Self {
        Self {
            graph: DiGraph::new(),
            indices: HashMap::new(),
        }
    }
}

impl DependencyTracker {
    /// Clears the tracker state
    pub fn clear(&mut self) {
        self.graph.clear();
        self.indices.clear();
    }

    /// Inserts a new node for `child_hash` and creates edges from each given
    /// parent hash (if found) to that child node. If a parent hash is not yet
    /// in the DAG, no edge is created for it.
    ///
    /// # Arguments
    /// * `child_hash` - The transaction hash of the new child transaction.
    /// * `parent_hashes` - A list of known parent transaction hashes in the DAG.
    pub fn add_transaction(
        &mut self,
        child_hash: TransactionHash,
        parent_hashes: &[TransactionHash],
    ) {
        let child_idx = self.graph.add_node(child_hash);
        self.indices.insert(child_hash, child_idx);

        for phash in parent_hashes {
            if let Some(&pidx) = self.indices.get(phash) {
                // Add edge from parent to child
                self.graph.add_edge(pidx, child_idx, ());
            }
        }
    }

    /// Removes the node for `tx_hash` if it exists, maintaining graph integrity
    /// by connecting parent nodes directly to child nodes.
    ///
    /// # Arguments
    /// * `tx_hash` - The transaction hash to remove from the DAG.
    pub fn remove_transaction(&mut self, tx_hash: &TransactionHash) {
        if let Some(idx) = self.indices.remove(tx_hash) {
            // Identify affected nodes before removal
            let parents: Vec<NodeIndex> = self
                .graph
                .neighbors_directed(idx, Direction::Incoming)
                .collect();

            let children: Vec<NodeIndex> = self
                .graph
                .neighbors_directed(idx, Direction::Outgoing)
                .collect();

            // Remove the node
            self.graph.remove_node(idx);

            // Create direct edges from parents to children
            // This preserves the transitive dependency relationship
            for parent in &parents {
                for child in &children {
                    // Check if nodes still exist in the graph
                    if self.graph.node_weight(*parent).is_some()
                        && self.graph.node_weight(*child).is_some()
                    {
                        // Avoid creating duplicate edges
                        if self
                            .graph
                            .edges_connecting(*parent, *child)
                            .next()
                            .is_none()
                        {
                            self.graph.add_edge(*parent, *child, ());
                        }
                    }
                }
            }

            // Rebuild indices since node indices may have changed
            self.rebuild_indices();
        }
    }

    /// Rebuilds the indices map after graph structure changes
    fn rebuild_indices(&mut self) {
        self.indices.clear();
        for idx in self.graph.node_indices() {
            if let Some(hash) = self.graph.node_weight(idx) {
                self.indices.insert(*hash, idx);
            }
        }
    }

    /// Returns all transitive descendants of `tx_hash` by traversing outgoing edges.
    /// Each returned TransactionHash is reachable from the given node in a
    /// parent -> child direction. The node itself is excluded from the returned list.
    ///
    /// # Arguments
    /// * `tx_hash` - A transaction hash whose descendants we want to find.
    pub fn get_descendants(&self, tx_hash: &TransactionHash) -> Vec<TransactionHash> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        if let Some(&start_idx) = self.indices.get(tx_hash) {
            queue.push_back(start_idx);
        }

        while let Some(current) = queue.pop_front() {
            for edge in self.graph.edges_directed(current, Direction::Outgoing) {
                let child_idx = edge.target();
                if visited.insert(child_idx) {
                    if let Some(child_hash) = self.graph.node_weight(child_idx) {
                        result.push(*child_hash);
                    }
                    queue.push_back(child_idx);
                }
            }
        }
        result
    }

    /// Returns all transitive ancestors of `tx_hash` by traversing incoming edges
    /// (child -> parent direction). The node itself is excluded from the returned list.
    ///
    /// # Arguments
    /// * `tx_hash` - A transaction hash whose ancestors we want to find.
    pub fn get_ancestors(&self, tx_hash: &TransactionHash) -> Vec<TransactionHash> {
        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        if let Some(&start_idx) = self.indices.get(tx_hash) {
            queue.push_back(start_idx);
        }

        while let Some(current) = queue.pop_front() {
            for edge in self.graph.edges_directed(current, Direction::Incoming) {
                let parent_idx = edge.source();
                if visited.insert(parent_idx) {
                    if let Some(parent_hash) = self.graph.node_weight(parent_idx) {
                        result.push(*parent_hash);
                    }
                    queue.push_back(parent_idx);
                }
            }
        }
        result
    }

    /// Gathers all ancestors (including the starting node) from the dependency graph.
    pub fn gather_ancestors(&self, start: NodeIndex) -> Vec<NodeIndex> {
        let mut result = Vec::new();
        let mut stack = vec![start];
        let mut visited = HashSet::new();
        while let Some(n) = stack.pop() {
            if visited.insert(n) {
                result.push(n);
                for edge in self.graph.edges_directed(n, Direction::Incoming) {
                    stack.push(edge.source());
                }
            }
        }
        result.reverse(); // Ensure parents come before children
        result
    }

    /// Returns a topologically sorted list of NodeIndex values from the dependency graph.
    /// Nodes are ordered so that each parent appears before its children.
    pub fn topological_order(&self) -> Vec<NodeIndex> {
        let mut visited = HashSet::new();
        let mut stack = Vec::new();
        for &idx in self.indices.values() {
            if !visited.contains(&idx) {
                self.dfs_topo(idx, &mut visited, &mut stack);
            }
        }
        stack.reverse();
        stack
    }

    /// A recursive depth-first search helper for topological ordering.
    fn dfs_topo(
        &self,
        node: NodeIndex,
        visited: &mut HashSet<NodeIndex>,
        stack: &mut Vec<NodeIndex>,
    ) {
        visited.insert(node);
        for edge in self.graph.edges_directed(node, Direction::Outgoing) {
            let target = edge.target();
            if !visited.contains(&target) {
                self.dfs_topo(target, visited, stack);
            }
        }
        stack.push(node);
    }

    /// Retrieves the transaction hash stored at the given node index in the dependency graph.
    pub fn get_tx_by_node(&self, node: NodeIndex) -> Option<TransactionHash> {
        self.graph.node_weight(node).cloned()
    }
}
