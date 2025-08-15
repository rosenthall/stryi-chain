use std::collections::HashMap;
use crate::block::{Block, BlockHash};

/// Metadata stored for **each** block that currently belongs to the
/// active (canonical) chain.  Only lightweight fields – no full UTXO.
#[derive(Clone, Debug, PartialEq)]
struct ChainIndexEntry {
    parent: BlockHash,
    height: u64,
    work:   u128, // cumulative work up to *this* block
}

#[derive(Clone, Debug, PartialEq)]
struct TipInfo {
    height: u64,
    hash:   BlockHash,
    work:   u128,
}

/// In‑memory index of the *active* chain.
///
/// * `entries` – metadata for every block in the main chain;
/// * `tip`     – cached best block for O(1) access.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct ChainIndex {
    entries: HashMap<BlockHash, ChainIndexEntry>,
    tip:     Option<TipInfo>,
}

impl ChainIndex {
    /// Insert/overwrite a block together with already‑calculated cumulative work.
    ///
    /// The caller must guarantee that `cumulative_work` is
    /// `work(parent) + 2^difficulty_bits`.
    pub fn insert(&mut self, block: &Block, cumulative_work: u128) {
        let hash = block.block_hash();

        self.entries.insert(
            hash,
            ChainIndexEntry {
                parent: block.header.previous_block_hash,
                height: block.header.height,
                work:   cumulative_work,
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

    /// Drop a block from the index.  If the removed block was tip –
    /// re‑scans the map to pick the next heaviest.
    pub fn remove(&mut self, hash: &BlockHash) -> bool {
        let existed = self.entries.remove(hash).is_some();
        if !existed {
            return false;
        }
        if let Some(t) = &self.tip {
            if &t.hash == hash {
                self.recompute_tip();
            }
        }
        true
    }

    /// O(1) – does the hash belong to the active chain?
    pub fn has(&self, hash: &BlockHash) -> bool {
        self.entries.contains_key(hash)
    }

    /// Parent hash if the block is tracked.
    pub fn parent(&self, hash: &BlockHash) -> Option<BlockHash> {
        self.entries.get(hash).map(|e| e.parent)
    }

    /// Height lookup.
    pub fn height(&self, hash: &BlockHash) -> Option<u64> {
        self.entries.get(hash).map(|e| e.height)
    }

    /// Cumulative work lookup.
    pub fn work(&self, hash: &BlockHash) -> Option<u128> {
        self.entries.get(hash).map(|e| e.work)
    }

    /// Current best tip.
    pub fn tip(&self) -> Option<(u64, BlockHash, u128)> {
        self.tip.as_ref().map(|t| (t.height, t.hash, t.work))
    }

    /// Ancestor of given height (inclusive).  Walks parents until the
    /// requested height is reached.
    pub fn ancestor_of_height(&self, mut hash: BlockHash, target: u64) -> Option<BlockHash> {
        while let Some(entry) = self.entries.get(&hash) {
            if entry.height == target {
                return Some(hash);
            }
            hash = entry.parent;
        }
        None
    }

    /// Lowest common ancestor of two blocks *within* the active chain.
    pub fn lca(&self, mut a: BlockHash, mut b: BlockHash) -> Option<BlockHash> {
        let mut ha = self.height(&a)?;
        let mut hb = self.height(&b)?;

        // align heights
        while ha > hb {
            a = self.parent(&a)?;
            ha -= 1;
        }
        while hb > ha {
            b = self.parent(&b)?;
            hb -= 1;
        }
        // walk in lock‑step
        while a != b {
            a = self.parent(&a)?;
            b = self.parent(&b)?;
        }
        Some(a)
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
        self.tip = Some(TipInfo { height: entry.height, hash, work: entry.work });
    }
}

#[cfg(test)]
mod tests {
    use crate::merkletree::MerkleHash;
    use super::*;
    use crate::block::{BlockData, BlockHeader};

    fn make_block(parent: BlockHash, height: u64, difficulty_bits: u8) -> Block {
        let mut block = Block {
            header: BlockHeader {
                version: 1,
                merkle_root_hash: MerkleHash::empty(),
                previous_block_hash: parent,
                height,
                difficulty_bits,
                timestamp: 0,
                nonce: 0,
                is_genesis: height == 0,
            },
            data: BlockData { transactions: vec![] },
        };
        block.update_merkle_root();
        block
    }

    #[test]
    fn chain_index_tip_and_has_and_ancestor() {
        let mut idx = ChainIndex::default();
        // genesis
        let g = make_block(BlockHash::empty(), 0, 4);
        let w_g = 1u128 << 4;
        idx.insert(&g, w_g);
        assert_eq!(idx.tip().unwrap(), (0, g.block_hash(), w_g));
        assert!(idx.has(&g.block_hash()));

        // A extends genesis
        let a = make_block(g.block_hash(), 1, 6);
        let w_a = w_g + (1u128 << 6);
        idx.insert(&a, w_a);
        assert_eq!(idx.tip().unwrap(), (1, a.block_hash(), w_a));

        // B fork with lower work – tip stays A
        let b = make_block(g.block_hash(), 1, 5);
        let w_b = w_g + (1u128 << 5);
        idx.insert(&b, w_b);
        assert_eq!(idx.tip().unwrap(), (1, a.block_hash(), w_a));

        // ancestor lookup A → genesis
        assert_eq!(idx.ancestor_of_height(a.block_hash(), 0).unwrap(), g.block_hash());
    }

    #[test]
    fn chain_index_remove_recomputes_tip() {
        let mut idx = ChainIndex::default();
        let g = make_block(BlockHash::empty(), 0, 4);
        let w_g = 1u128 << 4;
        idx.insert(&g, w_g);
        let a = make_block(g.block_hash(), 1, 6);
        let w_a = w_g + (1u128 << 6);
        idx.insert(&a, w_a);

        // remove tip (a) → tip should fall back to genesis
        idx.remove(&a.block_hash());
        assert_eq!(idx.tip().unwrap(), (0, g.block_hash(), w_g));
    }

    #[test]
    fn chain_index_lca_basic() {
        // build two branches sharing genesis
        let mut idx = ChainIndex::default();
        let g = make_block(BlockHash::empty(), 0, 4);
        idx.insert(&g, 1u128 << 4);

        let a1 = make_block(g.block_hash(), 1, 5);
        let w_a1 = (1u128 << 4) + (1u128 << 5);
        idx.insert(&a1, w_a1);
        let a2 = make_block(a1.block_hash(), 2, 5);
        idx.insert(&a2, w_a1 + (1u128 << 5));

        let b1 = make_block(g.block_hash(), 1, 6);
        let w_b1 = (1u128 << 4) + (1u128 << 6);
        idx.insert(&b1, w_b1);
        let b2 = make_block(b1.block_hash(), 2, 6);
        idx.insert(&b2, w_b1 + (1u128 << 6));
        let b3 = make_block(b2.block_hash(), 3, 6);
        idx.insert(&b3, w_b1 + (1u128 << 6) + (1u128 << 6));

        let lca = idx.lca(a2.block_hash(), b3.block_hash()).unwrap();
        assert_eq!(lca, g.block_hash());
    }
}
