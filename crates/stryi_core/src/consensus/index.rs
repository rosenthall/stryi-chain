use crate::block::{Block, BlockHash};


struct ChainIndexEntry {
    parent : BlockHash,
    height : u64,
    work : u64,
}

#[derive(Default)]
pub struct ChainIndex {
    // TODO: Implement  
}


impl ChainIndex {
    pub(crate) fn insert(&mut self, block: &Block, cumulative_work : u128) {
        todo!()
    }
}
