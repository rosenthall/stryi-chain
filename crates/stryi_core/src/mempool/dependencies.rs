use crate::transactions::TransactionHash;
use petgraph::Direction;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet, VecDeque};

/// Tracks parent-child relationships between mempool transactions.
#[derive(Default)]
pub struct DependencyTracker {
    graph: DiGraph<TransactionHash, ()>,
    indices: HashMap<TransactionHash, NodeIndex>,
}

impl DependencyTracker {
    pub fn clear(&mut self) {
        self.graph.clear();
        self.indices.clear();
    }

    pub fn add_transaction(
        &mut self,
        child_hash: TransactionHash,
        parent_hashes: &[TransactionHash],
    ) {
        let child_idx = self.graph.add_node(child_hash);
        self.indices.insert(child_hash, child_idx);

        for parent_hash in parent_hashes {
            if let Some(&parent_idx) = self.indices.get(parent_hash) {
                self.graph.add_edge(parent_idx, child_idx, ());
            }
        }
    }

    /// Removes one transaction and its mempool-only edges.
    pub fn remove_transaction(&mut self, tx_hash: &TransactionHash) {
        let Some(idx) = self.indices.remove(tx_hash) else {
            return;
        };

        self.graph.remove_node(idx);
        self.rebuild_indices();
    }

    pub fn get_descendants(&self, tx_hash: &TransactionHash) -> Vec<TransactionHash> {
        self.walk_hashes(tx_hash, Direction::Outgoing)
    }

    /// Returns the transaction and any mempool ancestors in parent-first order.
    pub fn get_with_ancestors(&self, tx_hash: &TransactionHash) -> Vec<TransactionHash> {
        let Some(&start) = self.indices.get(tx_hash) else {
            return Vec::new();
        };

        self.gather_with_ancestors(start)
            .into_iter()
            .filter_map(|idx| self.graph.node_weight(idx).copied())
            .collect()
    }

    /// Returns all tracked transactions in topological order.
    pub fn topological_order(&self) -> Vec<TransactionHash> {
        let mut visited = HashSet::new();
        let mut stack = Vec::new();

        for idx in self.graph.node_indices() {
            if !visited.contains(&idx) {
                self.dfs_topo(idx, &mut visited, &mut stack);
            }
        }

        stack.reverse();
        stack
            .into_iter()
            .filter_map(|idx| self.graph.node_weight(idx).copied())
            .collect()
    }

    fn walk_hashes(&self, tx_hash: &TransactionHash, direction: Direction) -> Vec<TransactionHash> {
        let Some(&start_idx) = self.indices.get(tx_hash) else {
            return Vec::new();
        };

        let mut result = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::from([start_idx]);

        while let Some(current) = queue.pop_front() {
            for edge in self.graph.edges_directed(current, direction) {
                let next = match direction {
                    Direction::Outgoing => edge.target(),
                    Direction::Incoming => edge.source(),
                };

                if visited.insert(next) {
                    if let Some(hash) = self.graph.node_weight(next) {
                        result.push(*hash);
                    }
                    queue.push_back(next);
                }
            }
        }

        result
    }

    fn gather_with_ancestors(&self, start: NodeIndex) -> Vec<NodeIndex> {
        let mut result = Vec::new();
        let mut stack = vec![start];
        let mut visited = HashSet::new();

        while let Some(node) = stack.pop() {
            if visited.insert(node) {
                result.push(node);
                for edge in self.graph.edges_directed(node, Direction::Incoming) {
                    stack.push(edge.source());
                }
            }
        }

        result.reverse();
        result
    }

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

    fn rebuild_indices(&mut self) {
        self.indices.clear();

        for idx in self.graph.node_indices() {
            if let Some(hash) = self.graph.node_weight(idx) {
                self.indices.insert(*hash, idx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DependencyTracker;
    use crate::transactions::TransactionHash;

    fn hash(byte: u8) -> TransactionHash {
        TransactionHash::new(&[byte; 32])
    }

    #[test]
    fn removing_a_transaction_does_not_rewire_children() {
        let mut tracker = DependencyTracker::default();
        let parent = hash(1);
        let middle = hash(2);
        let child = hash(3);

        tracker.add_transaction(parent, &[]);
        tracker.add_transaction(middle, &[parent]);
        tracker.add_transaction(child, &[middle]);

        tracker.remove_transaction(&middle);

        assert!(tracker.get_descendants(&parent).is_empty());
        assert_eq!(tracker.get_with_ancestors(&child), vec![child]);
    }
}
