use crate::block::BlockHash;
use crate::consensus::classify::{BlockDisposition, KnownLocation};
use crate::consensus::forks::registry::{ForkRegistry, ForksRead};
use crate::consensus::index::ChainIndex;
use crate::consensus::validator::BlockValidator;
use crate::consensus::{ConsensusVerdict, FullNodeStorage};
use crate::difficulty::DifficultyCalc;
use crate::error::{StorageLayer, StryiCoreError};
use crate::transactions::UtxoProcessor;
use crate::{
    block::Block,
    consensus::{ConsensusConsts, ConsensusEngine},
};
use comfy_table::{Table, presets::ASCII_FULL};
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, trace};

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
        difficulty_calculator: DifficultyCalc<DB>,
    ) -> Result<Self, StryiCoreError> {
        info!("Initializing consensus engine...");

        let chain_index = Self::build_chain_index(db.clone()).await?;
        let forks = ForkRegistry::new();

        let engine = Self {
            consensus_consts,
            block_validator,
            difficulty_calculator,
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

            write_db.put_block(&block).await?;

            let undo = self
                .utxo_processor
                .apply_block(&block, &mut *write_db)
                .await?;

            write_db.put_block_undo(hash, undo).await?;
        }

        // update index
        self.chain_index.insert(&block, cumulative_work);

        Ok(ConsensusVerdict::Applied {
            new_chain_complexity: cumulative_work as u64,
        })
    }

    /// Handle a block that creates a fork from the canonical chain.
    async fn handle_creates_fork_from_canonical(
        &mut self,
        _block: Block,
        _lca: BlockHash,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        todo!("implement handle_creates_fork_from_canonical")
    }

    /// Handle a block that extends an existing fork.
    async fn handle_extends_fork(
        &mut self,
        _block: Block,
        _fork_root: BlockHash,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        todo!("implement handle_extends_fork")
    }
}

impl<DB: FullNodeStorage> ConsensusEngine for StryiConsensusEngine<DB> {
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
                BlockDisposition::Orphan { parent: _parent } => todo!("implement orphan handling"),
            }
        })
    }
}
