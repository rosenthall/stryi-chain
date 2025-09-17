use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::block::{Block, BlockHash};
use crate::error::StryiCoreError;

/// A single, in‑memory container for forked blocks that are **valid**
/// but **not** on the canonical chain at the moment.
#[derive(Debug, Default)]
pub struct ForkTree {
    /// hash -> full entry
    entries: HashMap<BlockHash, ForkEntry>,

    /// height -> set of hashes (helps pruning and quick stats)
    by_height: HashMap<u64, HashSet<BlockHash>>,
}

#[derive(Debug, Clone)]
pub struct ForkEntry {
    pub block: Block,
    pub cumulative_difficulty: u128,
    pub common_ancestor: BlockHash,
    pub timestamp: Instant,
}

impl ForkTree {
    /// Insert or overwrite a fork entry.
    pub fn put(&mut self, entry: ForkEntry) -> Result<(), StryiCoreError> {
        let hash = entry.block.block_hash();
        // if overwriting, clean up the old height index first
        if let Some(old) = self.entries.remove(&hash) {
            if let Some(set) = self.by_height.get_mut(&old.block.header.height) {
                set.remove(&hash);
                if set.is_empty() {
                    self.by_height.remove(&old.block.header.height);
                }
            }
        }
        self.by_height
            .entry(entry.block.header.height)
            .or_default()
            .insert(hash);
        self.entries.insert(hash, entry);
        Ok(())
    }

    /// Fetch an entry by its block hash.
    pub fn get(&self, hash: &BlockHash) -> Option<ForkEntry> {
        self.entries.get(hash).cloned()
    }

    /// Remove an entire side‑branch starting from `tip_hash` until we
    /// encounter a block that is **not** stored here (i.e. ancestor in main chain).
    pub fn prune_branch(&mut self, tip_hash: &BlockHash) {
        let mut cur = *tip_hash;
        while let Some(entry) = self.entries.remove(&cur) {
            if let Some(set) = self.by_height.get_mut(&entry.block.header.height) {
                set.remove(&cur);
                if set.is_empty() {
                    self.by_height.remove(&entry.block.header.height);
                }
            }
            cur = entry.block.header.previous_block_hash;
            if !self.entries.contains_key(&cur) {
                break;
            }
        }
    }

    /// Return hashes that currently have **no child** inside this tree –
    /// i.e. tips of every side‑branch we track.
    pub fn tips(&self) -> Vec<BlockHash> {
        let mut has_parent = HashSet::<BlockHash>::with_capacity(self.entries.len());
        for e in self.entries.values() {
            has_parent.insert(e.block.header.previous_block_hash);
        }
        self.entries
            .keys()
            .filter(|h| !has_parent.contains(*h))
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockData, BlockHeader};
    use crate::merkletree::MerkleHash;

    fn make_block(parent: BlockHash, height: u64) -> Block {
        let mut block = Block {
            header: BlockHeader {
                version: 1,
                merkle_root_hash: MerkleHash::empty(),
                previous_block_hash: parent,
                height,
                difficulty_bits: 4,
                timestamp: 0,
                nonce: 0,
                genesis_state: None,
            },
            data: BlockData {
                transactions: vec![],
            },
        };
        block.update_merkle_root();
        block
    }

    fn entry(block: Block, work: u128, ancestor: BlockHash) -> ForkEntry {
        ForkEntry {
            block,
            cumulative_difficulty: work,
            common_ancestor: ancestor,
            timestamp: Instant::now(),
        }
    }

    #[test]
    fn forktree_put_get_and_tips() {
        let mut ft = ForkTree::default();
        let g = make_block(BlockHash::empty(), 0);
        let a1 = make_block(g.block_hash(), 1);
        let a2 = make_block(a1.block_hash(), 2);

        ft.put(entry(a1.clone(), 100, g.block_hash())).unwrap();
        ft.put(entry(a2.clone(), 200, g.block_hash())).unwrap();

        assert!(ft.get(&a1.block_hash()).is_some());
        assert!(ft.get(&a2.block_hash()).is_some());

        let tips = ft.tips();
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0], a2.block_hash());
    }

    #[test]
    fn forktree_prune_branch_removes_chain() {
        let mut ft = ForkTree::default();
        let g = make_block(BlockHash::empty(), 0);
        let b1 = make_block(g.block_hash(), 1);
        let b2 = make_block(b1.block_hash(), 2);

        ft.put(entry(b1.clone(), 100, g.block_hash())).unwrap();
        ft.put(entry(b2.clone(), 200, g.block_hash())).unwrap();

        ft.prune_branch(&b2.block_hash());
        assert!(ft.get(&b1.block_hash()).is_none());
        assert!(ft.get(&b2.block_hash()).is_none());
        assert!(ft.tips().is_empty());
    }
}
