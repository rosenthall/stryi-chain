use crate::block::{Block, BlockHash};
use crate::consensus::forks::registry::ForksRead;
use crate::consensus::{FullNodeStorage, StryiConsensusEngine};
use std::fmt::{Display, Formatter};
use tracing::debug;

/// Result of routing an incoming block relative to the local chain state.
/// This enum is purely topological, and
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::consensus) enum BlockDisposition {
    /// Block hash already known locally
    Known { location: KnownLocation },

    /// Block linearly extends the canonical chain (parent == main tip)
    ExtendsCanonical,

    /// Block forks off the canonical chain
    CreatesForkFromCanonical { lca: BlockHash },

    /// Block extends an existing fork branch
    ExtendsFork {
        parent: BlockHash,
        fork_root: BlockHash,
    },

    /// Block cannot be connected (unknown parent)
    Orphan { parent: BlockHash },
}

/// Describes where a block is known to be already stored: either main chain or fork tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(in crate::consensus) enum KnownLocation {
    CanonicalChain,
    ForkTree,
}

impl Display for KnownLocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            KnownLocation::CanonicalChain => f.write_str("canonical chain"),
            KnownLocation::ForkTree => f.write_str("fork tree"),
        }
    }
}

impl<DB: FullNodeStorage> StryiConsensusEngine<DB> {
    /// Performs classification of the disposition of a block in the consensus process.
    /// This is a pure routing step: no validation, no I/O.
    pub(in crate::consensus) fn classify_block(&self, block: &Block) -> BlockDisposition {
        let hash = block.block_hash();
        let parent = block.header.previous_block_hash;

        debug!(
            hash = %hash,
            parent = %parent,
            height = block.header.height,
            "classify_block"
        );

        // fast check if the block is already in the main chain
        if self.chain_index.has(&hash) {
            debug!("classify_block: block already in canonical chain");
            return BlockDisposition::Known {
                location: KnownLocation::CanonicalChain,
            };
        }

        // check if the block is already known in the fork tree
        if self.forks.has(&hash) {
            debug!("classify_block: block already known in fork tree");
            return BlockDisposition::Known {
                location: KnownLocation::ForkTree,
            };
        }

        // check if the block linearly extends the canonical tip
        if let Some((_, tip_hash, _)) = self.chain_index.tip()
            && parent == tip_hash
        {
            debug!("classify_block: block extends canonical tip");
            return BlockDisposition::ExtendsCanonical;
        }

        // forks from the canonical chain (parent is in the main chain, but not tip)
        if self.chain_index.has(&parent) {
            debug!("classify_block: block forks from canonical chain");
            return BlockDisposition::CreatesForkFromCanonical { lca: parent };
        }

        // check if the block extends an existing fork
        if let Some(entry) = self.forks.get(&parent) {
            debug!("classify_block: block extends existing fork");
            return BlockDisposition::ExtendsFork {
                parent,
                fork_root: entry.common_ancestor,
            };
        }

        // If neither of the above conditions are met, the block is considered an orphan.
        debug!("classify_block: block is orphan");
        BlockDisposition::Orphan { parent }
    }
}
