use crate::block::BlockHash;
use crate::consensus::ConsensusVerdict;
use crate::consensus::forks::forktree::{ForkEntry, ForkTree};
use crate::consensus::forks::overlay::ForkDbOverlay;
use crate::consensus::index::ChainIndex;
use crate::consensus::validator::BlockValidator;
use crate::difficulty::DifficultyCalc;
use crate::error::{StorageLayer, StryiCoreError};
use crate::storage::{BlockStorage, StorageStats, UndoStorage, UtxoStorage};
use crate::transactions::UtxoProcessor;
use crate::{
    block::Block,
    consensus::{ConsensusConsts, ConsensusEngine},
};
use comfy_table::{Table, presets::ASCII_FULL};
use futures::future::BoxFuture;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, info, trace};

// StryiConsensusEngine is responsible for validating and processing blocks according to the consensus rules.
///
/// It is generic over two types parameters:
/// - `DB` (database) that implements the `UtxoStorage`, `BlockStorage` and `StorageStats` traits. This enables the engine
///   to work with any storage backend that conforms to the interface (e.g. InMemoryUtxoStorage, StryiStorage, etc.).
/// - `FS` (Stands for Forks Storage) that implements `ForkStorage` trait. It allows engine use different backends
///   for storing and maintaining forks tree.
pub struct StryiConsensusEngine<DB>
where
    DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage + 'static,
{
    /// Consensus static rules object defining parameters like adjustment intervals.
    pub(crate) consensus_consts: ConsensusConsts,

    /// Block validator used to verify block-level properties such as proof-of-work, merkle root correctness,
    /// coinbase placement, transaction dependencies and ordering
    pub(crate) block_validator: BlockValidator<DB>,

    /// Difficulty calculator function.
    pub(crate) difficulty_calculator: DifficultyCalc<DB>,

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
    /// allowing quick reorganization if this branch becomes "better" than main one.
    pub(crate) forks: ForkTree,
}

impl<DB: UtxoStorage + BlockStorage + StorageStats + UndoStorage> StryiConsensusEngine<DB> {
    /// Creates a fully wired consensus engine.
    ///
    /// * Scans the current best chain in `db` to build `ChainIndex`.
    /// * Leaves `forks` empty; side branches appear as `on_block` is called.
    pub async fn new(
        consensus_consts: ConsensusConsts,
        block_validator: BlockValidator<DB>,
        utxo_processor: UtxoProcessor,
        db: Arc<RwLock<DB>>,
        difficulty_calculator: DifficultyCalc<DB>,
    ) -> Result<Self, StryiCoreError> {
        info!("Initializing consensus engine...");

        let chain_index = Self::build_chain_index(db.clone()).await?;

        let engine = Self {
            consensus_consts,
            block_validator,
            difficulty_calculator,
            utxo_processor,
            db: db.clone(),
            chain_index,
            forks: ForkTree::default(),
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

    fn on_block(&mut self, block: Block) -> BoxFuture<'_, Result<ConsensusVerdict, Self::Error>> {
        let block = block.clone();

        // generate a simple table log for this block's most important fields

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
            let block_hash = block.block_hash();
            let hash = block_hash;

            // Check if the block is already in main chain
            if self.chain_index.has(&hash) {
                debug!("Block {} is already in the main chain", hash);
                return Ok(ConsensusVerdict::AlreadyIncludedInChain);
            }

            // And if in fork
            if self.forks.get(&hash).is_some() {
                debug!("Block {} is already known in fork tree", hash);
                return Ok(ConsensusVerdict::AlreadyKnownInForkTree);
            }

            // locate parent and determine its origin
            let parent = block.header.previous_block_hash;
            let parent_in_main = self.chain_index.has(&parent);
            let parent_in_fork = self.forks.get(&parent);

            // if parent is unknown, reject as orphan
            if !parent_in_main && parent_in_fork.clone().is_none() {
                debug!(
                    "Parent {} of block {} is unknown, rejecting as orphan",
                    parent, hash
                );
                return Ok(ConsensusVerdict::Rejected(StryiCoreError::other(
                    "Unknown parent block - orphan",
                )));
            }

            // log that we have found the block and its parent and origin
            debug!(
                "Parent {} of block {} found in {}",
                parent,
                hash,
                if parent_in_main {
                    "main chain"
                } else {
                    "fork tree"
                }
            );

            // Validate the block against the appropriate storage view
            let validation_result = if parent_in_main {
                // Parent is in the main chain - validate against canonical DB
                let read_db = self.db.read().await;
                self.block_validator.validate(&block, &*read_db).await
            } else {
                // Parent is in the fork tree, so we need to validate against fork overlay
                // For now, we'll create an overlay and validate
                let fork_entry = parent_in_fork.clone().expect("parent must be in fork tree");
                let parent_work = fork_entry.cumulative_difficulty;

                // Create a fork overlay starting from the fork's common ancestor
                // NOTE: ForkOverlay, even theoretically, cannot change real persistent storage, since it only has read lock.
                let db = self.db.read().await;

                let overlay = ForkDbOverlay::new(&*db, parent_work);

                // TODO: Need to replay fork blocks onto overlay before validation
                self.block_validator.validate(&block, &overlay).await
            };

            match validation_result {
                Ok(()) => {
                    debug!("Block {} passed validation", hash);
                }
                Err(e) => {
                    debug!("Block {} failed validation: {:?}", hash, e);
                    return Ok(ConsensusVerdict::Rejected(e));
                }
            }

            // Calculate cumulative difficulty for this block
            let block_work = 1u128 << block.header.difficulty_bits;
            let cumulative_work = if parent_in_main {
                self.chain_index.work(&parent).unwrap() + block_work
            } else {
                parent_in_fork.clone().unwrap().cumulative_difficulty + block_work
            };

            // Determine what to do based on fork choice
            if parent_in_main {
                // Block extends main chain directly
                let current_tip_work = self.chain_index.tip().map(|(_, _, w)| w).unwrap_or(0);

                if cumulative_work > current_tip_work {
                    // This block becomes new tip - apply it
                    debug!("Block {} extends main chain and becomes new tip", hash);

                    // Apply block to storage
                    let mut write_db = self.db.write().await;

                    // Store the block
                    write_db.put_block(&block).await.map_err(|e| {
                        StryiCoreError::storage(
                            StorageLayer::Block,
                            format!("Failed to store block: {e:?}"),
                        )
                    })?;

                    // Apply transactions to UTXO set and get undo data
                    let undo = self
                        .utxo_processor
                        .apply_block(&block, &mut *write_db)
                        .await
                        .map_err(|e| {
                            StryiCoreError::storage(
                                StorageLayer::Utxo,
                                format!("Failed to apply block transactions: {e:?}"),
                            )
                        })?;

                    // Store undo data
                    write_db.put_block_undo(hash, undo).await.map_err(|e| {
                        StryiCoreError::storage(
                            StorageLayer::Undo,
                            format!("Failed to store undo data: {e:?}"),
                        )
                    })?;

                    drop(write_db);

                    // Update chain index
                    self.chain_index.insert(&block, cumulative_work);

                    debug!(
                        "Applied block {} at height {} to main chain",
                        hash, block.header.height
                    );

                    return Ok(ConsensusVerdict::Applied {
                        new_chain_complexity: cumulative_work as u64,
                    });
                }
            }

            // Block creates or extends a fork
            let common_ancestor = if parent_in_main {
                parent
            } else {
                parent_in_fork.clone().unwrap().common_ancestor
            };

            self.forks.put(ForkEntry {
                block: block.clone(),
                cumulative_difficulty: cumulative_work,
                common_ancestor,
                timestamp: Instant::now(),
            })?;

            debug!(
                "Buffered block {} into fork tree with cumulative work {}",
                hash, cumulative_work
            );

            // Check if this fork should trigger reorganization
            let current_tip_work = self.chain_index.tip().map(|(_, _, w)| w).unwrap_or(0);

            let lca_height = self
                .chain_index
                .height(&common_ancestor)
                .ok_or_else(|| StryiCoreError::other("Common ancestor not in chain index"))?;

            if cumulative_work > current_tip_work {
                // This fork is now heavier - perform reorganization
                info!(
                    "Fork containing block {} has higher work ({} > {}), triggering reorganization",
                    hash, cumulative_work, current_tip_work
                );

                // Collect blocks to detach and attach
                let (detach, attach) = self.collect_detach_attach(&block, common_ancestor);

                debug!(
                    "Reorganization: detaching {} blocks, attaching {} blocks",
                    detach.len(),
                    attach.len()
                );

                // Perform the reorganization
                let mut write_db = self.db.write().await;

                // Detach blocks from main chain (undo in reverse order)
                for &block_hash in &detach {
                    let undo = write_db
                        .get_block_undo(block_hash)
                        .await
                        .map_err(|e| {
                            StryiCoreError::storage(
                                StorageLayer::Undo,
                                format!("Failed to get undo data: {e:?}"),
                            )
                        })?
                        .ok_or_else(|| StryiCoreError::other("Missing undo data for block"))?;

                    self.utxo_processor
                        .rewind_block(undo, &mut *write_db)
                        .await
                        .map_err(|e| {
                            StryiCoreError::storage(
                                StorageLayer::Utxo,
                                format!("Failed to revert block: {e:?}"),
                            )
                        })?;

                    // Remove from chain index
                    self.chain_index.remove(&block_hash);
                }
                // Attach new fork blocks
                for attach_block in &attach {
                    let attach_hash = attach_block.block_hash();

                    // Store block
                    write_db.put_block(attach_block).await.map_err(|e| {
                        StryiCoreError::storage(
                            StorageLayer::Block,
                            format!("Failed to store block during reorg: {e:?}"),
                        )
                    })?;

                    // Apply transactions
                    let undo = self
                        .utxo_processor
                        .apply_block(attach_block, &mut *write_db)
                        .await
                        .map_err(|e| {
                            StryiCoreError::storage(
                                StorageLayer::Utxo,
                                format!("Failed to apply block during reorg: {e:?}"),
                            )
                        })?;

                    // Store undo
                    write_db
                        .put_block_undo(attach_hash, undo)
                        .await
                        .map_err(|e| {
                            StryiCoreError::storage(
                                StorageLayer::Undo,
                                format!("Failed to store undo during reorg: {e:?}"),
                            )
                        })?;

                    // Calculate work for this block
                    let prev_work = self
                        .chain_index
                        .work(&attach_block.header.previous_block_hash)
                        .unwrap_or(0);
                    let work = prev_work + (1u128 << attach_block.header.difficulty_bits);

                    // Update chain index
                    self.chain_index.insert(attach_block, work);
                }

                drop(write_db);

                // Prune the fork branch that just became main
                self.forks.prune_branch(&hash);

                // Build deleted blocks map
                let mut deleted_blocks = std::collections::HashMap::new();
                for &h in &detach {
                    if let Some(height) = self.chain_index.height(&h) {
                        deleted_blocks.insert(height, h);
                    }
                }

                info!("Reorganization completed successfully");

                return Ok(ConsensusVerdict::CausedReorganization { deleted_blocks });
            }

            // Fork is buffered but not heavy enough to trigger reorg
            Ok(ConsensusVerdict::BufferedIntoForkTree {
                common_ancestor_height: (common_ancestor, lca_height),
            })
        })
    }
}
