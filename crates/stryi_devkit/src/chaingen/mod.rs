use crate::chaingen::state::GenerationState;
pub use config::*;
use std::sync::Arc;

/// Configuration struct for ChainGen.
mod config;

/// Transaction generation helpers.
mod txgen;

/// Definition and helpers for Seed struct used in ChainGen.
mod seed;

/// UTXO tracking and selection helpers.
mod utxo;

/// Seed schedule: mapping from height to active seed.
/// Used to create ChaCha8Rng instances for block synthesis.
mod seed_schedule;

/// Generation state: accounts, UTXOs, etc.
mod state;

/// Structures/functionality related to funding accounts.
mod funding_account;

/// Initialization helpers
mod init;

/// Defines `TxGenerationStrategy`, a helper for choosing how to generate transactions.
mod strategy;

/// Block generation logic.
mod blockgen;

use crate::chaingen::seed_schedule::SeedSchedule;

use funding_account::FundAccount;
use stryi_core::consensus::{
    ConsensusConsts, ConsensusEngine, ConsensusVerdict, StryiConsensusEngine,
};
use stryi_core::storage::{BlockStorage, StorageStats};
use stryi_storage::StryiStorage;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Generator for synthetic blockchain data.
/// Builds and persists a chain of blocks according to the provided configuration.
// TODO: Make ChainGenerator use ConsensusConsts from genesis.
pub struct ChainGenerator {
    /// Config for this run.
    config: ChainGenConfig,

    /// Fund account
    pub(crate) fund_account: FundAccount,

    /// Height of the highest before the generation started.
    pre_generation_height: u64,

    /// Consensus constants from genesis.
    consensus_consts: ConsensusConsts,

    /// Optional consensus engine instance.
    /// Only being initialized and used if config.blocks.persistence_mode == PersistenceMode::ConsensusEngine
    consensus_engine: Option<StryiConsensusEngine<StryiStorage>>,

    /// Storage instance.
    storage: Arc<RwLock<StryiStorage>>,
}

impl ChainGenerator {
    /// Deterministic, sequential generation: build and persist N blocks after the latest block.
    /// Uses the preferred way of persistence, direct insert or consensus engine, as per config.
    /// Initializes SeedSchedule and reseeds exactly at switch heights.
    pub async fn start(mut self) -> Result<(), String> {
        info!("Starting chain generation and persistence...");

        // Number of post-genesis blocks to generate.
        // If a chain already has blocks, num_blocks means "blocks after current tip".
        let total_to_generate = self.config.chain.num_blocks;
        if total_to_generate == 0 {
            info!("Nothing to generate.");
            return Ok(());
        }

        // chain height where distributor will be placed
        let distributor_height = self.pre_generation_height + 1;
        // the first normal block should start after distributor
        let normal_blocks = distributor_height + 1;
        // last chain height we will generate to (inclusive)
        let end_height = self.pre_generation_height + total_to_generate;

        // Initialize the seed schedule and the first active seed/rng.
        let schedule = SeedSchedule::new(&self.config.chain.seed);
        let mut current_seed = schedule.seed_at(distributor_height);
        let mut rng = SeedSchedule::rng_from_seed(current_seed);

        info!(
            "Seed schedule initialized. First active seed: {}",
            current_seed
        );

        // Pre-generate all accounts deterministically using the initial seed
        // And build generation_state
        info!("Building GenerationState");
        let mut generation_state = GenerationState::new_with_random_accounts(
            self.config.blocks.active_addresses_count as usize,
            self.fund_account.clone(),
            current_seed,
            self.pre_generation_height,
        );

        // log current top addresses and some stuff
        {
            info!("Analyzing chain before generation start..");

            let s = self.storage.read().await;
            generation_state
                .log_utxos_state(&s, true)
                .await
                .map_err(|e| e.to_string())?;
        }

        // print all generated accounts
        generation_state
            .accounts
            .iter()
            .map(|acc| acc.0) // only iterate addresses
            .enumerate()
            .for_each(|(idx, address)| info!("[{}] {address}", idx + 1));

        info!(
            "Generated {} deterministic accounts",
            &generation_state.accounts.len()
        );

        // Insert block that distributes balances
        // distributing block always will be first after the pre-generation state.
        info!("Building distributing block");

        info!(
            "Distribution block will be placed at height #{}.",
            distributor_height
        );

        let distributor = self.build_distributing_block(&mut generation_state).await?;
        debug!("Distributor block : {:#?}", distributor);

        // Persist distribution block with a short-lived write lock
        // If the consensus engine is configured -> call on_block (do not hold storage lock).
        // Otherwise -> direct insert into storage.
        let dist_utxo_count = distributor.data.transactions[1].data.outputs.len();
        if let Some(engine) = &mut self.consensus_engine {
            // try to apply the block
            match engine
                .on_block(distributor.clone())
                .await
                .map_err(|e| format!("Consensus engine failed: {:?}", e))?
            {
                // Applied successfully
                ConsensusVerdict::Applied {
                    new_chain_complexity,
                } => {
                    info!(
                        "Distributor persisted via ConsensusEngine. new_chain_complexity={}",
                        new_chain_complexity
                    );
                }

                // Rejected by consensus
                ConsensusVerdict::Rejected(err) => {
                    return Err(format!(
                        "Distributor block rejected by consensus: {:?}",
                        err
                    ));
                }

                other => {
                    error!(
                        "Unexpected ConsensusVerdict ({:?}) for distributor block: {:#?}",
                        other, distributor
                    );
                    return Err("Unexpected ConsensusVerdict".to_string());
                }
            }
        } else {
            // direct insert (keep a short-lived lock)
            let mut storage_guard = self.storage.write().await;
            storage_guard
                .put_block(&distributor)
                .await
                .map_err(|e| format!("Cannot insert distribution block : {:?}", e))?;
        }

        // Apply the block to update the state
        generation_state.apply_block(&distributor);

        info!(
            "|->  persisted Distributor Block (seed {}) on height {}, it created {} outputs",
            current_seed, distributor_height, dist_utxo_count
        );
        /* TODO: inserting undo here if flag passed */

        // acc for tx count; include distributor transactions if you want to count them
        let mut tx_count = distributor.data.transactions.len();

        // Main generation loop
        for height in normal_blocks..=end_height {
            debug!("Building block at height {}...", height);

            // If this exact chain height is a switch point, reseed before building the block.
            if schedule.is_switch_height(height) {
                current_seed = schedule.seed_at(height);
                rng = SeedSchedule::rng_from_seed(current_seed);
                info!("Switching seed at height {} -> {}", height, current_seed);
            }

            let block = self
                .build_block(height, &mut rng, &mut generation_state)
                .await
                .map_err(|e| format!("failed to build block at chain height {} : {}", height, e))?;

            // Persist block depending on persistence mode.
            if let Some(engine) = &mut self.consensus_engine {
                let blk_for_engine = block.clone();
                match engine
                    .on_block(blk_for_engine)
                    .await
                    .map_err(|e| format!("Consensus engine failed at height {}: {:?}", height, e))?
                {
                    // Applied successfully
                    ConsensusVerdict::Applied {
                        new_chain_complexity,
                    } => {
                        debug!(
                            "Block at height {} persisted via ConsensusEngine. new_chain_complexity={}",
                            height, new_chain_complexity
                        );
                    }

                    // Rejected by consensus
                    ConsensusVerdict::Rejected(err) => {
                        return Err(format!(
                            "Block at height {} rejected by consensus: {:?}",
                            height, err
                        ));
                    }

                    other => {
                        warn!(
                            "Unexpected ConsensusVerdict ({:?}) for block at height {}: {:?}",
                            other, height, block
                        );
                        return Err("Unexpected ConsensusVerdict".to_string());
                    }
                }
            }
            // DIRECT INSERT
            else {
                // Acquire write lock only for the short duration required to persist the block
                let mut storage_guard = self.storage.write().await;
                storage_guard
                    .put_block(&block)
                    .await
                    .map_err(|e| format!("put_block failed at height {}: {}", height, e))?;
            }

            // log current utxos state
            let utxos_amount = |s: &GenerationState| {
                s.account_utxos
                    .iter()
                    .map(|(_addr, set)| set.iter().count())
                    .sum::<usize>()
            };

            debug!(
                "There is {} UTXOs available before persisting block {}",
                utxos_amount(&generation_state),
                height
            );

            // update the generation state
            generation_state.apply_block(&block);

            // and log after
            debug!(
                "There is {} UTXOs available after persisting block {}",
                utxos_amount(&generation_state),
                height
            );

            // Update counter
            tx_count += block.data.transactions.len();

            let blocks_done = height - normal_blocks + 1;
            let avg_tx_count = blocks_done as f64 / tx_count as f64;

            // insert undo if needed
            // note: if config is set to PersistenceMode::ConsensusEngine, this will be done by consensus engine automatically.
            if self.config.blocks.need_undo
                && self.config.chain.persistence_mode == PersistenceMode::DirectInsert
            {
                // NOTE: Do we really need that?
                todo!("Fix inserting BlockUndo in chaingen if `need_undo` flag provided.");
            }

            if blocks_done.is_multiple_of(10) || height == end_height {
                info!(
                    "|->  persisted {}/{} blocks (seed {}). total transactions : {}, per block(avg) : {:.2}",
                    blocks_done,
                    // total_to_generate includes distributor, so subtract 1 here to show number of normal blocks
                    total_to_generate.saturating_sub(1),
                    current_seed,
                    tx_count,
                    avg_tx_count
                );
            }
        }

        generation_state.save_accounts(
            self.config.chain.output_path.clone(),
            self.pre_generation_height,
            end_height,
        )?;

        // log current top addresses and some stuff
        {
            let s = self.storage.read().await;
            generation_state
                .log_utxos_state(&s, false)
                .await
                .map_err(|e| e.to_string())?;

            info!(
                "Done. Persisted {} blocks. With total transactions created: {}",
                total_to_generate, tx_count
            );
        }

        // TODO: Finalize chaingen run: e.g., write summary file with some stats, etc.

        Ok(())
    }
}
