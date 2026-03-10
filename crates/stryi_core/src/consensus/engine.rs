use crate::block::BlockHash;
use crate::consensus::classify::{BlockDisposition, KnownLocation};
use crate::consensus::forks::overlay::ForkDbOverlay;
use crate::consensus::forks::registry::{ForkEntry, ForkRegistry, ForksRead, ForksWrite};
use crate::consensus::index::ChainIndex;
use crate::consensus::validator::BlockValidator;
use crate::consensus::{BlockStorage, ConsensusVerdict, FullNodeStorage, UndoStorage};
use crate::error::{StorageLayer, StryiCoreError};
use crate::transactions::UtxoProcessor;
use crate::{
    block::Block,
    consensus::{ConsensusConsts, ConsensusEngine},
};
use comfy_table::{Table, presets::ASCII_FULL};
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, info, trace, warn};

// StryiConsensusEngine is responsible for validating and processing blocks according to the consensus rules.
pub struct StryiConsensusEngine<DB>
where
    DB: FullNodeStorage,
{
    /// Consensus static rules object defining parameters like adjustment intervals.
    pub(crate) consensus_consts: ConsensusConsts,

    /// Block validator used to verify block-level properties such as proof-of-work, merkle root correctness,
    /// coinbase placement, transaction dependencies and ordering
    pub(crate) block_validator: BlockValidator<DB>,

    /// UTXO processor that applies transactions within a block to update the UTXO set.
    // TODO: consider renaming it later, maybe in TransactionsProcessor? Current name is a little weird
    pub(crate) utxo_processor: UtxoProcessor,

    /// Shared handle to the underlying storage wrapped in an `RwLock`.
    /// The `Arc` allows cheap cloning across subsystems, while the
    /// `RwLock` lets concurrent readers proceed without blocking each
    /// other and still grants exclusive access for writes when the
    /// consensus engine needs it.
    pub(crate) db: Arc<RwLock<DB>>,

    /// Fork registry that tracks all the known forks in the chain.
    pub(crate) forks: ForkRegistry,

    /// Lightweight in-memory index of the *active* chain.
    ///
    /// Maps each block hash to its height and cumulative work, giving O(1)
    /// ancestor look-ups and fast fork-choice comparisons.
    pub(crate) chain_index: ChainIndex,
}

impl<DB: FullNodeStorage> StryiConsensusEngine<DB> {
    /// Creates a fully wired consensus engine.
    ///
    /// * Scans the current best chain in `db` to build `ChainIndex`.
    /// * Leaves `forks` empty; side branches appear as `on_block` is called.
    pub async fn new(
        consensus_consts: ConsensusConsts,
        block_validator: BlockValidator<DB>,
        utxo_processor: UtxoProcessor,
        db: Arc<RwLock<DB>>,
    ) -> Result<Self, StryiCoreError> {
        info!("Initializing consensus engine...");

        let chain_index = Self::build_chain_index(db.clone()).await?;
        let forks = ForkRegistry::new();

        let engine = Self {
            consensus_consts,
            block_validator,
            forks,
            utxo_processor,
            db: db.clone(),
            chain_index,
        };

        Ok(engine)
    }

    /// Prints a welcome message with the current consensus engine state and some settings.
    /// Meant to be called once on startup.
    pub fn startup_message(&self) {
        // print some info about consensus engine state
        info!("Welcome from Stryi Consensus Engine!");

        // print some info about consensus engine state
        info!("Current consensus engine state:");

        let mut table = Table::new();
        table.load_preset(ASCII_FULL);
        table.set_header(vec!["Key", "Value"]);

        // Database implementation
        let database_impl = std::any::type_name::<DB>();
        table.add_row(vec!["Database implementation", &database_impl]);

        // tip info
        let (tip_height, tip_hash, tip_work) = match self.chain_index.tip() {
            Some((h, hh, w)) => (h.to_string(), hh.to_string(), w.to_string()),
            None => ("N/A".to_string(), "N/A".to_string(), "0".to_string()),
        };

        table.add_row(vec!["Tip height", &tip_height]);
        table.add_row(vec!["Tip hash", &tip_hash]);
        table.add_row(vec!["Tip cumulative work", &tip_work]);

        // consensus consts (separately one by one)
        let (adjustment_interval_blocks, initial_subsidy, decay_interval, decay_step) = (
            self.consensus_consts.difficulty_adjustment_interval_blocks,
            self.consensus_consts.initial_subsidy,
            self.consensus_consts.decay_interval,
            self.consensus_consts.decay_step,
        );

        table.add_row(vec![
            "Difficulty adjustment interval (blocks)",
            &adjustment_interval_blocks.to_string(),
        ]);
        table.add_row(vec!["Initial subsidy", &initial_subsidy.to_string()]);
        table.add_row(vec!["Decay interval (blocks)", &decay_interval.to_string()]);
        table.add_row(vec!["Decay step", &decay_step.to_string()]);

        // Print table as a single info log
        info!("\n{}", table);
    }

    pub fn tip(&self) -> Option<(u64, BlockHash, u128)> {
        self.chain_index.tip()
    }

    /// Walks from the stored tip back to genesis and fills `ChainIndex`.
    /// May return an error if chain refers to unknown block, or if refers to block that has no BlockUndo saved
    async fn build_chain_index(db: Arc<RwLock<DB>>) -> Result<ChainIndex, StryiCoreError> {
        // hold lock on db
        let db = db.read().await;
        info!("Starting collecting chain index!");

        let mut blocks = Vec::new();

        // try to get the latest block
        let (_, mut cursor_hash) = db.tip().await.map_err(|e| {
            StryiCoreError::storage(
                StorageLayer::Block,
                format!("Cannot get tip block from storage: {e:#?}"),
            )
        })?;

        // iterate from the tip to the genesis
        loop {
            let block = match db.get_block_by_hash(cursor_hash).await {
                // Error
                Err(e) => Err(StryiCoreError::storage(
                    StorageLayer::Block,
                    format!(
                        "Unexpected error while trying to get block {cursor_hash} in storage: {e}"
                    ),
                )),
                // No error but no such block found
                Ok(None) => Err(StryiCoreError::storage(
                    StorageLayer::Block,
                    format!("Cannot find block {cursor_hash} in persistent storage"),
                )),
                // OK
                Ok(Some(block)) => Ok(block),
            }?;

            blocks.push(block.clone());

            // Check if this block has undo
            // NOTE: Maybe I should cache these at this point? For future reorgs or something
            if block.header.height != 0 {
                let _undo = match db.get_block_undo(block.block_hash()).await {
                    Err(e) => Err(StryiCoreError::storage(
                        StorageLayer::Block,
                        format!(
                            "Unexpected error while trying to get block's {cursor_hash} UndoData in storage: {e}"
                        ),
                    )),
                    Ok(None) => Err(StryiCoreError::storage(
                        StorageLayer::Block,
                        format!("Cannot find block's {cursor_hash} UndoData in persistent storage"),
                    )),
                    Ok(Some(undo)) => Ok(undo),
                }?;
            }

            // Stop when genesis
            if block.header.height == 0 {
                break;
            }

            cursor_hash = block.header.previous_block_hash;
        }

        blocks.reverse();

        // initialize index
        let mut index = ChainIndex::new();

        // calculate cumulative work and insert entries to the index
        let mut cumulative_work: u128 = 0;
        for block in blocks {
            cumulative_work += 1u128 << block.header.difficulty_bits;
            index.insert(&block, cumulative_work);
        }

        Ok(index)
    }

    /// Return immediately if the block is already known in the main chain or fork tree
    async fn handle_known(
        &self,
        block: Block,
        location: KnownLocation,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        debug!(
            "Block {} is already known in {}.",
            block.block_hash(),
            location
        );

        Ok(match location {
            KnownLocation::CanonicalChain => ConsensusVerdict::AlreadyIncludedInChain,
            KnownLocation::ForkTree => ConsensusVerdict::AlreadyKnownInForkTree,
        })
    }

    /// Process a block that extends the canonical chain's tip.
    async fn handle_extend_canonical(
        &mut self,
        block: Block,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        let hash = block.block_hash();
        let parent = block.header.previous_block_hash;

        let (_tip_height, tip_hash, tip_work) = self.chain_index.tip().expect("Must be a tip");
        assert_eq!(tip_hash, parent);

        // validate
        let read_db = self.db.read().await;
        if let Err(e) = self.block_validator.validate(&block, &*read_db).await {
            return Ok(ConsensusVerdict::Rejected(e));
        }
        drop(read_db);

        // compute work
        let block_work = 1u128 << block.header.difficulty_bits;
        let cumulative_work = tip_work + block_work;

        // apply to storage
        {
            let mut write_db = self.db.write().await;

            write_db
                .put_block(&block)
                .await
                .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))?;

            let undo = self
                .utxo_processor
                .apply_block(&block, &mut *write_db)
                .await
                .map_err(|e| StryiCoreError::storage(StorageLayer::Utxo, e.to_string()))?;

            write_db
                .put_block_undo(hash, undo)
                .await
                .map_err(|e| StryiCoreError::storage(StorageLayer::Undo, e.to_string()))?;
        }

        // update index
        self.chain_index.insert(&block, cumulative_work);

        Ok(ConsensusVerdict::Applied {
            new_chain_complexity: cumulative_work as u64,
        })
    }

    /// Builds a transient `ForkDbOverlay` whose UTXO state reflects the
    /// canonical chain rewound to `lca`.
    ///
    /// It walks from the current canonical tip back to `lca`, calling
    /// `rewind_block` on the overlay for each block in between.  The
    /// canonical DB itself is **not** mutated - all rewind deltas live
    /// in the overlay's in-memory maps.
    async fn build_fork_overlay<'a>(
        &self,
        db: &'a DB,
        lca: BlockHash,
        lca_work: u128,
    ) -> Result<ForkDbOverlay<'a, DB>, StryiCoreError> {
        let mut overlay = ForkDbOverlay::new(db, lca_work);

        // collect block hashes from tip down to (but not including) the LCA
        let mut to_rewind = Vec::new();
        let (_, mut cursor, _) = self.chain_index.tip().expect("Must have a tip");
        while cursor != lca {
            to_rewind.push(cursor);
            cursor = self.chain_index.parent(&cursor).ok_or_else(|| {
                StryiCoreError::consensus_chain_selection(format!(
                    "Block {} has no parent in chain index during overlay build",
                    cursor
                ))
            })?;
        }

        // rewind each block on the overlay (tip-first order is correct for undo)
        for hash in &to_rewind {
            let undo = db
                .get_block_undo(*hash)
                .await
                .map_err(|e| StryiCoreError::storage(StorageLayer::Undo, e.to_string()))?
                .ok_or_else(|| {
                    StryiCoreError::storage(
                        StorageLayer::Undo,
                        format!("Missing BlockUndo for {} during overlay rewind", hash),
                    )
                })?;
            self.utxo_processor.rewind_block(undo, &mut overlay).await?;
        }

        Ok(overlay)
    }

    /// Handle a block that creates a fork from the canonical chain.
    /// The block's parent is in the canonical chain but is not the current tip.
    async fn handle_creates_fork_from_canonical(
        &mut self,
        block: Block,
        lca: BlockHash,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        let hash = block.block_hash();
        info!("Block {} creates fork from canonical at LCA {}", hash, lca);

        let lca_work = self.chain_index.work(&lca).ok_or_else(|| {
            StryiCoreError::consensus_chain_selection("LCA not found in chain index")
        })?;

        // build overlay rewound to LCA state, then validate against it
        {
            let read_db = self.db.read().await;
            let overlay = self.build_fork_overlay(&*read_db, lca, lca_work).await?;

            if let Err(e) = self
                .block_validator
                .validate_for_fork(&block, &*read_db, &overlay)
                .await
            {
                return Ok(ConsensusVerdict::Rejected(e));
            }
        }

        let block_work = 1u128 << block.header.difficulty_bits;
        let fork_work = lca_work + block_work;

        let (_, _, canonical_work) = self.chain_index.tip().expect("Must have a tip");

        if fork_work > canonical_work {
            debug!("Fork immediately heavier than canonical, triggering reorg");
            self.perform_reorg(lca, vec![block]).await
        } else {
            debug!("Fork weaker than canonical, buffering");
            self.forks.insert(ForkEntry {
                tip: hash,
                cumulative_work: fork_work,
                common_ancestor: lca,
                timestamp: Instant::now(),
                blocks: vec![block],
            });
            Ok(ConsensusVerdict::Buffered)
        }
    }

    /// Handle a block that extends an existing fork.
    /// The block's parent is the tip of an existing fork in the registry.
    async fn handle_extends_fork(
        &mut self,
        block: Block,
        fork_root: BlockHash,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        let hash = block.block_hash();
        let parent = block.header.previous_block_hash;
        info!(
            "Block {} extends fork (root={}, parent={})",
            hash, fork_root, parent
        );

        // look up the existing fork by its current tip (= this block's parent)
        let entry = self.forks.get(&parent).ok_or_else(|| {
            StryiCoreError::consensus_chain_selection("Fork entry not found for parent")
        })?;

        let lca = entry.common_ancestor;
        let lca_work = self.chain_index.work(&lca).ok_or_else(|| {
            StryiCoreError::consensus_chain_selection("Fork LCA not found in chain index")
        })?;

        // build overlay rewound to LCA, replay existing fork blocks, then validate new block
        {
            let read_db = self.db.read().await;
            let mut overlay = self.build_fork_overlay(&*read_db, lca, lca_work).await?;

            for fork_block in &entry.blocks {
                self.utxo_processor
                    .apply_block(fork_block, &mut overlay)
                    .await?;
            }

            // header validated against canonical DB (difficulty is height-based),
            // transactions validated against fork-local UTXO state
            if let Err(e) = self
                .block_validator
                .validate_for_fork(&block, &*read_db, &overlay)
                .await
            {
                return Ok(ConsensusVerdict::Rejected(e));
            }
        }

        // compute updated fork work
        let block_work = 1u128 << block.header.difficulty_bits;
        let fork_work = entry.cumulative_work + block_work;

        // build updated blocks list
        let mut fork_blocks = entry.blocks.clone();
        fork_blocks.push(block.clone());

        // remove old entry, check reorg
        self.forks.remove(&parent);

        let (_, _, canonical_work) = self.chain_index.tip().expect("Must have a tip");

        if fork_work > canonical_work {
            debug!("Extended fork now heavier than canonical, triggering reorg");
            self.perform_reorg(lca, fork_blocks).await
        } else {
            debug!("Extended fork still weaker than canonical, buffering");
            self.forks.insert(ForkEntry {
                tip: hash,
                cumulative_work: fork_work,
                common_ancestor: lca,
                timestamp: Instant::now(),
                blocks: fork_blocks,
            });
            Ok(ConsensusVerdict::Buffered)
        }
    }

    /// Performs a chain reorganization: rewinds the canonical chain back to `lca`,
    /// then applies `fork_blocks` forward on top of it.
    async fn perform_reorg(
        &mut self,
        lca: BlockHash,
        fork_blocks: Vec<Block>,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        info!(
            "Performing reorg: rewinding to LCA {}, then applying {} fork block(s)",
            lca,
            fork_blocks.len()
        );

        let mut write_db = self.db.write().await;
        let lca_work = self.chain_index.work(&lca).ok_or_else(|| {
            StryiCoreError::consensus_chain_selection("LCA work not found in chain index")
        })?;

        let mut deleted_blocks: HashMap<u64, BlockHash> = HashMap::new();
        let mut to_rewind: Vec<BlockHash> = Vec::new();
        {
            let (_, mut cursor, _) = self.chain_index.tip().expect("Must have a tip");
            while cursor != lca {
                let height = self.chain_index.height(&cursor).ok_or_else(|| {
                    StryiCoreError::consensus_chain_selection(format!(
                        "Block {} not found in chain index during reorg rewind",
                        cursor
                    ))
                })?;
                deleted_blocks.insert(height, cursor);
                to_rewind.push(cursor);
                cursor = self.chain_index.parent(&cursor).ok_or_else(|| {
                    StryiCoreError::consensus_chain_selection(format!(
                        "Block {} has no parent in chain index during reorg",
                        cursor
                    ))
                })?;
            }
        }

        // Build the reorg against an overlay, then commit the delta in one pass.
        let delta = {
            let mut overlay = ForkDbOverlay::new(&*write_db, lca_work);

            for hash in &to_rewind {
                let undo = write_db
                    .get_block_undo(*hash)
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Undo, e.to_string()))?
                    .ok_or_else(|| {
                        StryiCoreError::storage(
                            StorageLayer::Undo,
                            format!("Missing BlockUndo for {} during reorg", hash),
                        )
                    })?;

                self.utxo_processor
                    .rewind_block(undo, &mut overlay)
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Utxo, e.to_string()))?;
            }

            info!(
                "Rewound {} canonical block(s) (overlay)",
                deleted_blocks.len()
            );

            for block in &fork_blocks {
                let hash = block.block_hash();
                overlay
                    .put_block(block)
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))?;

                let undo = self
                    .utxo_processor
                    .apply_block(block, &mut overlay)
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Utxo, e.to_string()))?;

                overlay
                    .put_block_undo(hash, undo)
                    .await
                    .map_err(|e| StryiCoreError::storage(StorageLayer::Undo, e.to_string()))?;
            }

            overlay.into_delta()
        };

        delta.commit(&mut *write_db).await?;

        for hash in &to_rewind {
            self.chain_index.remove(hash);
        }

        let mut cumulative_work = lca_work;
        for block in &fork_blocks {
            cumulative_work += 1u128 << block.header.difficulty_bits;
            self.chain_index.insert(block, cumulative_work);
        }

        drop(write_db);

        info!(
            "Reorg complete: applied {} fork block(s), new tip work={}",
            fork_blocks.len(),
            cumulative_work
        );

        Ok(ConsensusVerdict::CausedReorganization { deleted_blocks })
    }
}

impl<DB: FullNodeStorage> ConsensusEngine for StryiConsensusEngine<DB> {
    type Error = StryiCoreError;

    fn on_block(&mut self, block: Block) -> BoxFuture<'_, Result<ConsensusVerdict, Self::Error>> {
        let block = block.clone();

        debug!(
            "Received new block {} at height {} with difficulty bits {}",
            block.block_hash(),
            block.header.height,
            block.header.difficulty_bits
        );
        trace!(
            "on_block hash={} prev={} height={} | chain_has_parent={} fork_has_parent={} | chain_index_tip={:?}",
            block.block_hash(),
            block.header.previous_block_hash,
            block.header.height,
            self.chain_index.has(&block.header.previous_block_hash),
            self.forks.get(&block.header.previous_block_hash).is_some(),
            self.chain_index.tip()
        );

        Box::pin(async move {
            let block_disposition = self.classify_block(&block);
            debug!(
                "Block {} is classified as {:?}",
                block.block_hash(),
                block_disposition
            );

            match block_disposition {
                BlockDisposition::Known { location } => self.handle_known(block, location).await,
                BlockDisposition::ExtendsCanonical => self.handle_extend_canonical(block).await,
                BlockDisposition::CreatesForkFromCanonical { lca } => {
                    self.handle_creates_fork_from_canonical(block, lca).await
                }
                BlockDisposition::ExtendsFork {
                    parent: _parent,
                    fork_root: _fork_root,
                } => self.handle_extends_fork(block, _fork_root).await,
                BlockDisposition::Orphan { parent } => {
                    warn!(
                        "Orphan block {} (parent {} unknown), ignoring for now",
                        block.block_hash(),
                        parent
                    );
                    Ok(ConsensusVerdict::Buffered)
                }
            }
        })
    }
}
