use crate::block::BlockHash;
use crate::consensus::ConsensusOnBlockVerdict;
use crate::consensus::index::ChainIndex;
use crate::consensus::validator::BlockValidator;
use crate::error::{StorageLayer, StryiCoreError};
use crate::forktree::ForkTree;
use crate::storage::{BlockStorage, StorageStats, UndoStorage, UtxoStorage};
use crate::transactions::UtxoProcessor;
use crate::{
    block::Block,
    consensus::{ConsensusConsts, ConsensusEngine},
};
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

#[cfg(test)]
use crate::storage::StryiInMemoryStorage;

// StryiConsensusEngine is responsible for validating and processing blocks according to the consensus rules.
///
/// It is generic over two types parameters:
/// - `DB` (database) that implements the `UtxoStorage`, `BlockStorage` and `StorageStats` traits. This enables the engine
/// to work with any storage backend that conforms to the interface (e.g. InMemoryUtxoStorage, StryiStorage, etc.).
/// - `FS` (Stands for Forks Storage) that implements `ForkStorage` trait. It allows engine use different backends
/// for storing and maintaining forks tree.
pub struct StryiConsensusEngine<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage,
{
    /// Consensus rules object defining parameters like current difficulty and adjustment intervals.
    pub(crate) rules: ConsensusConsts,

    /// Block validator used to verify block-level properties such as proof-of-work, merkle root correctness,
    /// coinbase placement, transaction dependencies and ordering
    pub(crate) block_validator: BlockValidator,

    /// UTXO processor that applies transactions within a block to update the UTXO set.
    // TODO: consider renaming it later, maybe in TransactionsProcessor? Current name is a little weird
    pub(crate) utxo_processor: UtxoProcessor,

    /// Shared handle to the underlying storage wrapped in an `RwLock`.
    /// The `Arc` allows cheap cloning across subsystems, while the
    /// `RwLock` lets concurrent readers proceed without blocking each
    /// other and still grants exclusive access for writes when the
    /// consensus engine needs it.
    pub(crate) db: Arc<RwLock<DB>>,

    /// Lightweight in-memory index of the *active* chain.
    ///
    /// Maps each block hash to its height and cumulative work, giving O(1)
    /// ancestor look-ups and fast fork-choice comparisons.
    pub(crate) chain_index: ChainIndex,

    /// Repository of all competing side branches.
    ///
    /// Stores blocks that are valid but have **not** yet won fork-choice.
    /// Each entry remembers cumulative work and the common ancestor,
    /// allowing quick reorganisation if this branch becomes "better" than main one.
    pub(crate) forks: ForkTree,
}

impl<DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage> StryiConsensusEngine<DB> {
    /// Creates a fully wired consensus engine.
    ///
    /// * Scans the current best chain in `db` to build `ChainIndex`.
    /// * Leaves `forks` empty; side branches appear as `on_block` is called.
    pub async fn new(
        rules: ConsensusConsts,
        block_validator: BlockValidator,
        utxo_processor: UtxoProcessor,
        db: Arc<RwLock<DB>>,
    ) -> Result<Self, StryiCoreError> {
        let chain_index = Self::build_chain_index(db.clone()).await?;

        Ok(Self {
            rules,
            block_validator,
            utxo_processor,
            db: db.clone(),
            chain_index,
            forks: ForkTree::default(),
        })
    }

    /// Walks from the stored tip back to genesis and fills `ChainIndex`.
    /// May return an error if chain refers to unknown block.
    async fn build_chain_index(db: Arc<RwLock<DB>>) -> Result<ChainIndex, StryiCoreError> {
        // Hold lock on db
        let db = db.read().await; // block_read?

        debug!("Starting collecting chain index");

        let mut index = ChainIndex::default();

        // Get latest block.
        let mut cursor_hash = db
            .tip()
            .await
            // Map error if any
            .map_err(|e| {
                StryiCoreError::storage(
                    StorageLayer::Block,
                    format!("Cannot get tip block from storage: {e:#?}"),
                )
            })?
            // (block_height, block_hash)
            .1;

        // acc
        let mut cumulative_work: u128 = 0;

        // Iterate over blocks one by one
        loop {
            match db.get_block_by_hash(cursor_hash).await {
                Ok(Some(block)) => {
                    cumulative_work += 1u128 << block.header.difficulty_bits;

                    // Add this block in index with specified cumulative work.
                    index.insert(&block, cumulative_work);

                    // reached genesis - success
                    if block.header.height == 0 {
                        break;
                    }

                    cursor_hash = block.header.previous_block_hash;
                }

                // Block not found -> error
                Ok(None) => {
                    return Err(StryiCoreError::storage(
                        StorageLayer::Block,
                        format!(
                            "Cannot find block {} in persistent storage to build chain index",
                            cursor_hash
                        ),
                    ));
                }

                // If got any error while traversing blocks -> return.
                Err(e) => {
                    return Err(StryiCoreError::storage(
                        StorageLayer::Block,
                        format!("{e:?}"),
                    ));
                }
            };
        }

        Ok(index)
    }

    /// Computes the cumulative chain work for a given slice of blocks.
    ///
    /// For each block, the work is defined as 2^(difficulty_bits).
    /// The total chain work is the sum of these values.
    /// This metric is used in chain selection algorithms to determine which fork is "heavier."
    // TODO: New trait + generic parameter for better computation of chain's difficulty?
    pub fn compute_chain_difficulty(&self, chain: &[Block]) -> u128 {
        let mut total = 0u128;
        for block in chain {
            let bits = block.header.difficulty_bits;
            total = total.saturating_add(1u128 << bits);
        }
        total
    }

    /// helper — cumulative work of parent + current diff bits
    fn calc_work(&self, parent_work: u128, diff_bits: u8) -> u128 {
        parent_work + (1u128 << diff_bits)
    }

    /// walks back from `from_hash` until it reaches `stop` (exclusive).
    /// returns (Vec<hashes_detached>, Vec<blocks_attached>)
    fn collect_detach_attach(
        &self,
        new_tip: &Block,
        lca: BlockHash,
    ) -> (Vec<BlockHash>, Vec<Block>) {
        // collect detach list (hashes in main chain)
        let mut detach = Vec::<BlockHash>::new();
        {
            let mut cur = self.chain_index.tip().unwrap().1;
            while cur != lca {
                detach.push(cur);
                cur = self.chain_index.parent(&cur).expect("parent must exist");
            }
        }

        // collect attach list from fork tree (blocks)
        let mut attach = Vec::<Block>::new();
        let mut walk = new_tip.clone();
        while walk.block_hash() != lca {
            attach.push(walk.clone());
            walk = self
                .forks
                .get(&walk.header.previous_block_hash)
                .expect("path must be in fork tree")
                .block;
        }

        attach.reverse();
        (detach, attach)
    }
}

impl<DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + 'static> ConsensusEngine
    for StryiConsensusEngine<DB>
{
    type Error = StryiCoreError;

    fn on_block(
        &mut self,
        block: Block,
    ) -> BoxFuture<Result<ConsensusOnBlockVerdict, Self::Error>> {
        let block = block.clone();

        Box::pin(async move {
            let block_hash = block.block_hash();
            let hash = block_hash;

            // Check if the block is already in main chain
            if self.chain_index.has(&hash) {
                return Ok(ConsensusOnBlockVerdict::AlreadyIncludedInChain);
            }

            // And if in fork
            if self.forks.get(&hash).is_some() {
                return Ok(ConsensusOnBlockVerdict::AlreadyKnownInForkTree);
            }

            // locate parent and return error if no
            let parent = block.header.previous_block_hash;
            let parent_in_main = self.chain_index.has(&parent);
            let parent_in_fork = self.forks.get(&parent);
            if !parent_in_main && parent_in_fork.is_none() {
                // orphan for now
                return Ok(ConsensusOnBlockVerdict::Rejected(StryiCoreError::other(
                    "Unknown parent.",
                )));
            }

            // if parent_in_main && parent == main_tip_hash {
            //     self.db
            //         .put_block(&block)
            //         .await
            //         .map_err(|e| StryiCoreError::storage(StorageLayer::Block, format!("{e:?}")))?;
            //     self.chain_index.insert(&block, cum_work);
            //     return Ok(ConsensusOnBlockVerdict::Applied { new_chain_complexity: cum_work as u64 });
            // }

            // TODO: Finish the `on_block` ASAP

            Err(Self::Error::other("unimplemented"))
        })
    }
}
