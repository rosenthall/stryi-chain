use crate::block::{Block, BlockHash};
use crate::consensus::classify::{BlockDisposition, KnownLocation};
use crate::consensus::forks::overlay::ForkDbOverlay;
use crate::consensus::forks::registry::{ForkEntry, ForkRegistry, ForksRead, ForksWrite};
use crate::consensus::index::ChainIndex;
use crate::consensus::validator::BlockValidator;
use crate::consensus::{
    CommittedTransition, ConsensusConsts, ConsensusEngine, ConsensusVerdict, FullNodeStorage,
};
use crate::difficulty::{ChainWork, add_work};
use crate::error::{StorageLayer, StryiCoreError};
use crate::storage::{
    ChainState, ChainTransition, StorageError, StorageRead, StorageStats, verified_block_work,
    verify_genesis_work_shape,
};
use crate::transactions::UtxoProcessor;
use comfy_table::{Table, presets::ASCII_FULL};
use std::sync::Arc;
use tracing::{debug, info, trace, warn};

fn canonical_corruption(layer: StorageLayer, details: impl Into<String>) -> StryiCoreError {
    StryiCoreError::storage(layer, StorageError::Corruption(details.into()))
}

/// Main consensus engine managing active chain state and competing forks.
/// Note: `on_block` calls must be serialized by the caller, and no external code
/// should mutate canonical chain state in `db` behind the engine's back.
pub struct StryiConsensusEngine<DB: FullNodeStorage> {
    /// Consensus static rules object defining parameters like the proof-of-work
    /// schedule and reward emission.
    pub(crate) consensus_consts: ConsensusConsts,

    /// Block validator used to verify block-level properties such as proof-of-work, merkle root correctness,
    /// coinbase placement, transaction dependencies and ordering
    pub(crate) block_validator: BlockValidator,

    /// UTXO processor that applies transactions within a block to update the UTXO set.
    // TODO: consider renaming it later, e.g. in TransactionsProcessor? Current name is a little weird
    pub(crate) utxo_processor: UtxoProcessor,

    pub(crate) db: Arc<DB>,

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
    pub fn new(
        consensus_consts: ConsensusConsts,
        block_validator: BlockValidator,
        utxo_processor: UtxoProcessor,
        db: Arc<DB>,
    ) -> Result<Self, StryiCoreError> {
        info!("Initializing consensus engine...");
        let chain_index = Self::build_chain_index(&db)?;

        Ok(Self {
            consensus_consts,
            block_validator,
            utxo_processor,
            db,
            forks: ForkRegistry::new(),
            chain_index,
        })
    }

    /// Prints a welcome message with the current consensus engine state and some settings.
    /// Meant to be called once on startup.
    pub fn startup_message(&self) {
        info!("Welcome from Stryi Consensus Engine!");
        info!("Current consensus engine state:");

        let mut table = Table::new();
        table.load_preset(ASCII_FULL);
        table.set_header(vec!["Key", "Value"]);

        let database_impl = std::any::type_name::<DB>();
        table.add_row(vec!["Database implementation", &database_impl]);

        let (tip_height, tip_hash, tip_work) = match self.chain_index.tip() {
            Some((h, hh, w)) => (h.to_string(), hh.to_string(), w.to_string()),
            None => ("N/A".to_string(), "N/A".to_string(), "0".to_string()),
        };

        table.add_row(vec!["Tip height", &tip_height]);
        table.add_row(vec!["Tip hash", &tip_hash]);
        table.add_row(vec!["Tip cumulative work", &tip_work]);

        let (initial_subsidy, decay_interval, decay_step) = (
            self.consensus_consts.initial_subsidy,
            self.consensus_consts.decay_interval,
            self.consensus_consts.decay_step,
        );

        table.add_row(vec![
            "PoW target spacing (seconds)",
            &self.consensus_consts.pow.target_spacing_secs.to_string(),
        ]);
        table.add_row(vec![
            "PoW half-life (seconds)",
            &self.consensus_consts.pow.half_life_secs.to_string(),
        ]);
        table.add_row(vec![
            "PoW initial hash threshold",
            &self.consensus_consts.pow.initial_hash_threshold.to_string(),
        ]);
        table.add_row(vec![
            "PoW max hash threshold",
            &self.consensus_consts.pow.max_hash_threshold.to_string(),
        ]);
        table.add_row(vec!["Initial subsidy", &initial_subsidy.to_string()]);
        table.add_row(vec!["Decay interval (blocks)", &decay_interval.to_string()]);
        table.add_row(vec!["Decay step", &decay_step.to_string()]);

        info!("\n{}", table);
    }

    pub fn tip(&self) -> Option<(u64, BlockHash, ChainWork)> {
        self.chain_index.tip()
    }

    fn validate_height(&self, block: &Block) -> bool {
        let parent = block.header.previous_block_hash;

        self.chain_index
            .height(&parent)
            .or_else(|| {
                self.forks
                    .get(&parent)?
                    .blocks
                    .last()
                    .map(|b| b.header.height)
            })
            .is_none_or(|height| height.checked_add(1) == Some(block.header.height))
    }

    /// Walks from the stored tip back to genesis and fills `ChainIndex`.
    /// May return an error if chain refers to unknown block, or if refers to block that has no BlockUndo saved
    /// or if canonical height mappings, work indexes and chain state disagree.
    fn build_chain_index(db: &DB) -> Result<ChainIndex, StryiCoreError> {
        let view = db
            .read()
            .map_err(|error| StryiCoreError::storage(StorageLayer::Other, error))?;

        let state = view
            .chain_state()
            .map_err(|error| StryiCoreError::storage(StorageLayer::Stats, error))?
            .ok_or_else(|| {
                StryiCoreError::storage(StorageLayer::Stats, StorageError::Uninitialized)
            })?;

        // Both phases use one snapshot, including the final chain-state comparison.
        let blocks = Self::collect_canonical_ancestry(&view, state.latest_block)?;
        Self::rebuild_chain_index(&view, &state, blocks)
    }

    fn collect_canonical_ancestry<S: StorageRead>(
        view: &S,
        tip: (u64, BlockHash),
    ) -> Result<Vec<Block>, StryiCoreError> {
        let mut cursor = tip;
        let mut blocks = Vec::new();

        loop {
            let (height, hash) = cursor;
            let block = view
                .get_block_by_hash(hash)
                .map_err(|error| StryiCoreError::storage(StorageLayer::Block, error))?
                .ok_or_else(|| {
                    canonical_corruption(
                        StorageLayer::Block,
                        format!("Canonical block {hash} is absent"),
                    )
                })?;
            let at_height = view
                .get_block_hash_by_height(height)
                .map_err(|error| StryiCoreError::storage(StorageLayer::Block, error))?;

            if block.block_hash() != hash
                || block.header.height != height
                || at_height != Some(hash)
            {
                return Err(canonical_corruption(
                    StorageLayer::Block,
                    "Canonical ancestry and heights disagree",
                ));
            }

            verify_genesis_work_shape(&block)?;

            // NOTE: Maybe I should cache UNDOs at this point to avoid repeated storage reads ?
            if height > 0 {
                let undo = view
                    .get_block_undo(hash)
                    .map_err(|error| StryiCoreError::storage(StorageLayer::Undo, error))?;

                if undo.is_none() {
                    return Err(canonical_corruption(
                        StorageLayer::Undo,
                        format!("Canonical undo {hash} is absent"),
                    ));
                }
            }

            if height == 0 && !block.header.is_genesis() {
                return Err(canonical_corruption(
                    StorageLayer::Block,
                    "Height zero has no genesis marker",
                ));
            }

            let parent_hash = block.header.previous_block_hash;
            blocks.push(block);

            if height == 0 {
                break;
            }

            cursor = (height - 1, parent_hash);
        }

        blocks.reverse();
        Ok(blocks)
    }

    fn rebuild_chain_index<S: StorageRead>(
        view: &S,
        state: &ChainState,
        blocks: Vec<Block>,
    ) -> Result<ChainIndex, StryiCoreError> {
        let mut index = ChainIndex::new();

        let mut cumulative_work = ChainWork::ZERO;
        for block in blocks {
            cumulative_work = add_work(cumulative_work, verified_block_work(&block)?)?;

            let hash = block.block_hash();
            let saved_index = view
                .get_block_index(&hash)
                .map_err(|error| StryiCoreError::storage(StorageLayer::Block, error))?
                .ok_or_else(|| {
                    canonical_corruption(StorageLayer::Block, "Canonical block index is absent")
                })?;

            if saved_index.height != block.header.height
                || saved_index.parent_hash != block.header.previous_block_hash
                || saved_index.chain_work != cumulative_work
            {
                return Err(canonical_corruption(
                    StorageLayer::Block,
                    "Canonical block index disagrees with ancestry",
                ));
            }

            index.insert(&block, cumulative_work);
        }

        if cumulative_work != state.chain_work
            || state.latest_block.0.checked_add(1) != Some(state.blocks_count)
        {
            return Err(canonical_corruption(
                StorageLayer::Stats,
                "Chain state disagrees with canonical history",
            ));
        }

        Ok(index)
    }

    /// Return immediately if the block is already known in the main chain or fork tree
    fn handle_known(
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
    fn handle_extend_canonical(
        &mut self,
        block: Block,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        {
            let view = self
                .db
                .read()
                .map_err(|error| StryiCoreError::storage(StorageLayer::Other, error))?;

            if let Err(error) = self.block_validator.validate(&block, &view) {
                return match error {
                    StryiCoreError::StorageError { .. } => Err(error),
                    other => Ok(ConsensusVerdict::Rejected(other)),
                };
            }
        }

        let (_, _, tip_work) = self.chain_index.tip().expect("Initialized chain has a tip");
        let cumulative_work = add_work(tip_work, verified_block_work(&block)?)?;

        let transition = self
            .db
            .apply_transition(ChainTransition::Append { block: &block })
            .map_err(|error| StryiCoreError::storage(StorageLayer::Other, error))?;

        // Publish the in-memory tip only after the complete storage transition succeeds.
        self.chain_index.insert(&block, cumulative_work);

        Ok(ConsensusVerdict::Applied {
            transition: CommittedTransition {
                new_head: transition.new_head,
                chain_work: transition.chain_work,
                attached: vec![block],
                detached: transition.detached,
            },
        })
    }

    /// Builds a transient `ForkDbOverlay` whose UTXO state reflects the
    /// canonical chain rewound to `lca`.
    ///
    /// It walks from the current canonical tip back to `lca`, calling
    /// `rewind_block` on the overlay for each block in between. The
    /// canonical DB itself is **not** mutated - all rewind deltas live
    /// in the overlay's in-memory maps.
    fn build_fork_overlay<'a, S: StorageRead>(
        &self,
        view: &'a S,
        lca: BlockHash,
    ) -> Result<ForkDbOverlay<'a, S>, StryiCoreError> {
        let mut overlay = ForkDbOverlay::new(view, lca)
            .map_err(|error| StryiCoreError::storage(StorageLayer::Other, error))?;
        let (_, mut cursor, _) = self.chain_index.tip().expect("Initialized chain has a tip");

        // rewind each block on the overlay (tip-first order is correct for undo)
        while cursor != lca {
            overlay
                .rewind_block(&self.utxo_processor)
                .map_err(|error| StryiCoreError::storage(StorageLayer::Undo, error))?;

            cursor = self.chain_index.parent(&cursor).ok_or_else(|| {
                StryiCoreError::consensus_chain_selection(
                    "Missing parent while rewinding fork view",
                )
            })?;
        }

        Ok(overlay)
    }

    /// Handle a block that creates a fork from the canonical chain.
    /// The block's parent is in the canonical chain but is not the current tip.
    fn handle_creates_fork_from_canonical(
        &mut self,
        block: Block,
        lca: BlockHash,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        let hash = block.block_hash();
        info!("Block {} creates fork from canonical at LCA {}", hash, lca);

        let lca_work = self.chain_index.work(&lca).ok_or_else(|| {
            StryiCoreError::consensus_chain_selection("Fork ancestor is outside canonical history")
        })?;

        {
            let view = self
                .db
                .read()
                .map_err(|error| StryiCoreError::storage(StorageLayer::Other, error))?;
            let overlay = self.build_fork_overlay(&view, lca)?;

            if let Err(error) = self.block_validator.validate_for_fork(&block, &overlay) {
                return match error {
                    StryiCoreError::StorageError { .. } => Err(error),
                    other => Ok(ConsensusVerdict::Rejected(other)),
                };
            }
        }

        let fork_work = add_work(lca_work, verified_block_work(&block)?)?;
        let (_, _, canonical_work) = self.chain_index.tip().expect("Initialized chain has a tip");

        if fork_work > canonical_work {
            debug!("Fork immediately heavier than canonical, triggering reorg");
            self.perform_reorg(lca, vec![block])
        } else {
            debug!("Fork weaker than canonical, buffering");
            self.forks.insert(ForkEntry {
                tip: hash,
                cumulative_work: fork_work,
                common_ancestor: lca,
                blocks: vec![block],
            });

            Ok(ConsensusVerdict::Buffered)
        }
    }

    /// Handle a block that extends an existing fork.
    /// The block's parent is the tip of an existing fork in the registry.
    fn handle_extends_fork(
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

        let entry = self
            .forks
            .get(&parent)
            .ok_or_else(|| StryiCoreError::consensus_chain_selection("Fork parent is absent"))?;
        let lca = entry.common_ancestor;

        {
            let view = self
                .db
                .read()
                .map_err(|error| StryiCoreError::storage(StorageLayer::Other, error))?;
            let mut overlay = self.build_fork_overlay(&view, lca)?;

            // Predecessors that live only in the fork must be visible to
            // header history, so replay them into the overlay before validating.
            for fork_block in &entry.blocks {
                overlay
                    .replay_block(fork_block)
                    .map_err(|error| StryiCoreError::storage(StorageLayer::Other, error))?;
            }

            if let Err(error) = self.block_validator.validate_for_fork(&block, &overlay) {
                return match error {
                    StryiCoreError::StorageError { .. } => Err(error),
                    other => Ok(ConsensusVerdict::Rejected(other)),
                };
            }
        }

        let fork_work = add_work(entry.cumulative_work, verified_block_work(&block)?)?;

        let mut fork_blocks = entry.blocks;
        fork_blocks.push(block);

        let (_, _, canonical_work) = self.chain_index.tip().expect("Initialized chain has a tip");

        if fork_work > canonical_work {
            debug!("Extended fork now heavier than canonical, triggering reorg");
            let verdict = self.perform_reorg(lca, fork_blocks)?;
            self.forks.remove(&parent);

            Ok(verdict)
        } else {
            debug!("Extended fork still weaker than canonical, buffering");
            self.forks.remove(&parent);
            self.forks.insert(ForkEntry {
                tip: hash,
                cumulative_work: fork_work,
                common_ancestor: lca,
                blocks: fork_blocks,
            });

            Ok(ConsensusVerdict::Buffered)
        }
    }

    /// Performs a chain reorganization: rewinds the canonical chain back to `lca`,
    /// then applies `fork_blocks` forward on top of it.
    /// Storage commits the replacement atomically before the in-memory chain is changed.
    fn perform_reorg(
        &mut self,
        lca: BlockHash,
        fork_blocks: Vec<Block>,
    ) -> Result<ConsensusVerdict, StryiCoreError> {
        info!(
            "Performing reorg: rewinding to LCA {}, then applying {} fork block(s)",
            lca,
            fork_blocks.len()
        );

        let mut new_tip_work = self.chain_index.work(&lca).ok_or_else(|| {
            StryiCoreError::consensus_chain_selection("Fork ancestor is outside canonical history")
        })?;
        let mut branch_work = Vec::with_capacity(fork_blocks.len());

        // Sum the new branch before any write so an overflow cannot leave a
        // half-applied reorganization behind.
        for block in &fork_blocks {
            new_tip_work = add_work(new_tip_work, verified_block_work(block)?)?;
            branch_work.push(new_tip_work);
        }

        let delta = {
            let view = self
                .db
                .read()
                .map_err(|error| StryiCoreError::storage(StorageLayer::Other, error))?;
            let mut overlay = self.build_fork_overlay(&view, lca)?;

            for block in &fork_blocks {
                overlay
                    .replay_block(block)
                    .map_err(|error| StryiCoreError::storage(StorageLayer::Utxo, error))?;
            }

            overlay
                .into_delta()
                .map_err(|error| StryiCoreError::storage(StorageLayer::Utxo, error))?
        };

        let transition = self
            .db
            .apply_transition(ChainTransition::Reorganize { delta })
            .map_err(|error| StryiCoreError::storage(StorageLayer::Other, error))?;

        for (_, hash) in &transition.detached {
            self.chain_index.remove(hash);
        }

        for (block, work) in fork_blocks.iter().zip(branch_work) {
            self.chain_index.insert(block, work);
        }

        for entry in self.forks.all() {
            if !self.chain_index.has(&entry.common_ancestor)
                || entry
                    .blocks
                    .iter()
                    .any(|block| self.chain_index.has(&block.block_hash()))
            {
                self.forks.remove(&entry.tip);
            }
        }

        info!(
            "Reorg complete: applied {} fork block(s), new tip work={}",
            fork_blocks.len(),
            new_tip_work
        );

        Ok(ConsensusVerdict::CausedReorganization {
            transition: CommittedTransition {
                new_head: transition.new_head,
                chain_work: transition.chain_work,
                attached: fork_blocks,
                detached: transition.detached,
            },
        })
    }
}

impl<DB: FullNodeStorage> ConsensusEngine for StryiConsensusEngine<DB> {
    type Error = StryiCoreError;

    fn on_block(&mut self, block: Block) -> Result<ConsensusVerdict, Self::Error> {
        self.forks.maintenance();

        let hash = block.block_hash();
        debug!(
            "Received new block {} at height {} with hash threshold {}",
            hash, block.header.height, block.header.hash_threshold
        );
        trace!(
            "on_block hash={} prev={} height={} | chain_has_parent={} fork_has_parent={} | chain_index_tip={:?}",
            hash,
            block.header.previous_block_hash,
            block.header.height,
            self.chain_index.has(&block.header.previous_block_hash),
            self.forks.get(&block.header.previous_block_hash).is_some(),
            self.chain_index.tip()
        );

        // A marker at normal height would hash to the zero block hash and
        // claim zero genesis work; reject it before classification can
        // match it with the canonical genesis.
        if let Err(error) = verify_genesis_work_shape(&block) {
            return Ok(ConsensusVerdict::Rejected(error));
        }

        if !self.validate_height(&block) {
            return Ok(ConsensusVerdict::Rejected(
                StryiCoreError::ConsensusValidationFailed {
                    details: "Block height must follow its parent".into(),
                },
            ));
        }

        let disposition = self.classify_block(&block);
        debug!("Block {} is classified as {:?}", hash, disposition);

        match disposition {
            BlockDisposition::Known { location } => self.handle_known(block, location),
            BlockDisposition::ExtendsCanonical => self.handle_extend_canonical(block),
            BlockDisposition::CreatesForkFromCanonical { lca } => {
                self.handle_creates_fork_from_canonical(block, lca)
            }
            BlockDisposition::ExtendsFork {
                parent: _parent,
                fork_root,
            } => self.handle_extends_fork(block, fork_root),
            BlockDisposition::Orphan { parent } => {
                warn!(
                    "Orphan block {} (parent {} unknown), ignoring for now",
                    hash, parent
                );
                Ok(ConsensusVerdict::Buffered)
            }
        }
    }
}
