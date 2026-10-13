use crate::block::{Block, BlockHash};
use crate::difficulty::ChainWork;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
struct ChainIndexEntry {
    parent: BlockHash,
    height: u64,
    work: ChainWork, // cumulative work up to *this* block
}

#[derive(Clone, Debug, PartialEq)]
struct TipInfo {
    height: u64,
    hash: BlockHash,
    work: ChainWork,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ChainIndex {
    entries: HashMap<BlockHash, ChainIndexEntry>,
    tip: Option<TipInfo>,
}

impl ChainIndex {
    #[inline(always)]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            tip: None,
        }
    }

    /// Caller must guarantee that `cumulative_work` is
    /// `work(parent) + block.work()`.
    pub fn insert(&mut self, block: &Block, cumulative_work: ChainWork) {
        let hash = block.block_hash();

        self.entries.insert(
            hash,
            ChainIndexEntry {
                parent: block.header.previous_block_hash,
                height: block.header.height,
                work: cumulative_work,
            },
        );

        let replace_tip = match &self.tip {
            None => true,
            Some(t) => cumulative_work > t.work,
        };

        if replace_tip {
            self.tip = Some(TipInfo {
                height: block.header.height,
                hash,
                work: cumulative_work,
            });
        }
    }

    /// If the removed block was the tip, re-scans the map to pick the next heaviest.
    pub fn remove(&mut self, hash: &BlockHash) -> bool {
        let existed = self.entries.remove(hash).is_some();
        if !existed {
            return false;
        }
        if let Some(t) = &self.tip
            && &t.hash == hash
        {
            self.recompute_tip();
        }
        true
    }

    pub fn has(&self, hash: &BlockHash) -> bool {
        self.entries.contains_key(hash)
    }

    pub fn parent(&self, hash: &BlockHash) -> Option<BlockHash> {
        self.entries.get(hash).map(|e| e.parent)
    }

    pub fn height(&self, hash: &BlockHash) -> Option<u64> {
        self.entries.get(hash).map(|e| e.height)
    }

    pub fn work(&self, hash: &BlockHash) -> Option<ChainWork> {
        self.entries.get(hash).map(|e| e.work)
    }

    pub fn tip(&self) -> Option<(u64, BlockHash, ChainWork)> {
        self.tip.as_ref().map(|t| (t.height, t.hash, t.work))
    }

    // internal helper
    fn recompute_tip(&mut self) {
        if self.entries.is_empty() {
            self.tip = None;
            return;
        }
        let (hash, entry) = self
            .entries
            .iter()
            .max_by_key(|(_, e)| e.work)
            .map(|(h, e)| (*h, e.clone()))
            .unwrap();
        self.tip = Some(TipInfo {
            height: entry.height,
            hash,
            work: entry.work,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{BlockData, BlockHeader, GenesisState};
    use crate::difficulty::{HashThreshold, add_work, block_work};
    use crate::merkletree::MerkleHash;

    fn make_block(parent: BlockHash, height: u64, hash_threshold: HashThreshold) -> Block {
        let mut block = Block {
            header: BlockHeader {
                version: 1,
                merkle_root_hash: MerkleHash::empty(),
                previous_block_hash: parent,
                height,
                hash_threshold,
                timestamp: 0,
                nonce: 0,
                genesis_state: (height == 0).then(GenesisState::default),
            },
            data: BlockData {
                transactions: vec![],
            },
        };
        block.update_merkle_root();
        block
    }

    #[test]
    fn chain_index_remove_recomputes_tip() {
        let mut idx = ChainIndex::new();

        // Genesis shape contributes zero work; the normal block adds its own.
        let g = make_block(BlockHash::empty(), 0, HashThreshold::ZERO);
        let w_g = ChainWork::ZERO;
        idx.insert(&g, w_g);

        let easy = HashThreshold::MAX >> 4;
        let a = make_block(g.block_hash(), 1, easy);
        let w_a = add_work(w_g, block_work(&easy)).unwrap();
        assert_eq!(w_a, ChainWork::from(16u64));
        idx.insert(&a, w_a);

        // remove tip (a) -> tip should fall back to genesis
        idx.remove(&a.block_hash());
        assert_eq!(idx.tip().unwrap(), (0, g.block_hash(), w_g));
    }
}
