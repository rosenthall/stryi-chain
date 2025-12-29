use crate::chaingen::state::GenerationState;
pub use config::*;
use indexmap::IndexSet;
use rand::Rng;
use rand_chacha::ChaCha8Rng;
use std::collections::HashSet;
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

/// Defines `TxGenerationStrategy`, a helper for choosing how to generate transactions.
mod strategy;

use crate::chaingen::seed_schedule::SeedSchedule;
use crate::chaingen::strategy::TxGenerationStrategy;
use crate::chaingen::txgen::{
    TransactionGenerationParams, generate_distributing_transaction, generate_transaction,
};
use crate::chaingen::utxo::UtxoInfo;
use funding_account::FundAccount;
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockData, BlockHash, meets_difficulty};
use stryi_core::consensus::{
    BlockValidator, ConsensusEngine, ConsensusVerdict, StorageStats, StryiConsensusEngine,
};
use stryi_core::difficulty::build_difficulty_calculator_from_consts;
use stryi_core::storage::{BlockStorage, UtxoStorage};
use stryi_core::transactions::{
    OutPoint, Transaction, TransactionData, TransactionKind, TransactionOut, UtxoProcessor,
};
use stryi_storage::{GenesisInitConfig, StorageStatus, StryiStorage};
use tokio::sync::RwLock;
use tracing::{debug, error, info, trace, warn};

/// Generator for synthetic blockchain data.
/// Builds and persists a chain of blocks according to the provided configuration.
// TODO: Make ChainGenerator use ConsensusConsts from genesis.
pub struct ChainGenerator {
    /// Config for this run.
    config: ChainGenConfig,

    /// Fund account
    pub(crate) fund_account: FundAccount,

    /// Height of the highest before the generation started.
    pre_generation_height: usize,

    /// Cached genesis block.
    genesis: Block,

    /// Optional consensus engine instance.
    /// Only being initialized and used if config.blocks.persistence_mode == PersistenceMode::ConsensusEngine
    consensus_engine: Option<StryiConsensusEngine<StryiStorage>>,

    /// Storage instance.
    storage: Arc<RwLock<StryiStorage>>,
}

impl ChainGenerator {
    /// Initialize ChainGenerator with given config.
    /// This includes reading and parsing the genesis config,
    /// and initializing storage.
    pub async fn initialize(config: ChainGenConfig) -> Result<Self, String> {
        info!("Initializing ChainGenerator...");

        // Check if output path exists, if not create it
        if !config.chain.output_path.is_dir() {
            std::fs::create_dir_all(&config.chain.output_path).map_err(|e| {
                format!(
                    "Failed to create output directory at {}: {}",
                    config.chain.output_path.display(),
                    e
                )
            })?;
        }

        // Read and parse the genesis config.
        let genesis_config: GenesisInitConfig = {
            let path = &config.chain.genesis_path;
            let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            serde_json::from_str::<GenesisInitConfig>(&content).map_err(|e| {
                format!(
                    "failed to parse genesis config at {}: {}",
                    path.display(),
                    e
                )
            })?
        };
        info!("Successfully read and parsed genesis config.");

        let storage_path = config.chain.output_path.clone();
        info!("Initializing storage at path: {}", storage_path.display());

        // Probe storage meta information
        let storage_status = StorageStatus::from_path(&storage_path).map_err(|e| {
            info!(
                "Failed to probe storage at {}: {e}",
                &storage_path.display()
            );
            e.to_string()
        })?;

        info!(
            "Storage status at {} => {:?}",
            &storage_path.display(),
            storage_status
        );

        // Ensure storage is uninitialized.
        if storage_status != StorageStatus::NoGenesis {
            return Err(format!(
                "Storage at {} is already initialized or in an invalid state: {:?}. Please choose an empty directory.",
                storage_path.display(),
                storage_status
            ));
        }

        // Initialize storage with genesis config.
        let storage = StryiStorage::initialize_in_path(storage_path, Some(genesis_config.clone()))
            .await
            .map_err(|e| e.to_string())?;

        info!("Storage initialized.");

        info!("Checking funding account.");

        let fund_account =
            FundAccount::load_from_private_key(config.clone().blocks.funding_key, &storage).await?;

        info!(
            "Fund account does exist in chain, and has balance ({})",
            fund_account.total_balance()
        );

        debug!(?fund_account);

        // read the latest block
        let (latest_block_height, latest_block_hash) = storage
            .tip()
            .await
            .map_err(|e| format!("Unable to get tip block from chain: {}", e))?;

        debug!(?latest_block_height, ?latest_block_hash);

        // read genesis
        // We will extract genesis tx from it for ConsensusConsts
        let genesis = storage
            .get_block_by_height(0)
            .await
            .map_err(|e| format!("Unable to read genesis block from storage! Error: {e}"))?
            .ok_or("Genesis block does not exist in storage after initialization")?;

        // filter out the genesis transaction
        let genesis_tx = genesis
            .data
            .transactions
            .first()
            .filter(|tx| tx.data.kind == TransactionKind::Genesis)
            .ok_or("Genesis block's first transaction must be of kind Genesis)")?;

        // Print all the allocations from the genesis block
        {
            let txid = genesis_tx.data.hash();
            for (vout, out) in genesis_tx.data.outputs.iter().enumerate() {
                info!(
                    "Genesis allocation - vout: {}, recipient: {}, value: {}",
                    vout, out.recipient, out.value
                );
            }

            // check that all these utxos are in the genesis utxo set
            for (vout, _out) in genesis_tx.data.outputs.iter().enumerate() {
                let outpoint = OutPoint {
                    txid,
                    vout: vout as u32,
                };
                let utxo = storage
                    .get_utxo(outpoint)
                    .await
                    .map_err(|e| format!("Failed to get UTXO from storage: {}", e))?;
                if utxo.is_none() {
                    return Err(format!(
                        "Genesis UTXO not found in storage for OutPoint: {:?}",
                        outpoint
                    ));
                }
            }
        }

        // build consensus engine if needed (in case if persistence_mode == ConsensusEngine)

        // Wrap db in arc and read-write lock
        let storage = Arc::new(RwLock::new(storage));

        let consensus_engine = match config.chain.persistence_mode {
            // if ConsensusEngine - build it
            PersistenceMode::ConsensusEngine => {
                info!(
                    "`PersistenceMode::ConsensusEngine` selected. Initializing StryiConsensusEngine..."
                );

                // read consensus consts from genesis
                let consensus_consts = genesis
                    .header
                    .genesis_state
                    .as_ref()
                    .ok_or("genesis_state is None in genesis")?
                    .consensus_consts;

                trace!(?consensus_consts);

                // build components of the engine

                let difficulty_calc = build_difficulty_calculator_from_consts(consensus_consts);
                let block_validator =
                    BlockValidator::new(consensus_consts, difficulty_calc.clone());
                let utxo_processor = UtxoProcessor::new();

                // .. and the engine itself
                let engine = StryiConsensusEngine::new(
                    consensus_consts,
                    block_validator,
                    utxo_processor,
                    storage.clone(),
                    difficulty_calc,
                )
                .await
                .map_err(|e| format!("Failed to initialize StryiConsensusEngine: {:?}", e))?;

                // print welcome message
                engine.startup_message();

                Some(engine)
            }
            // if DirectInsert - no engine
            PersistenceMode::DirectInsert => {
                warn!(
                    "`PersistenceMode::DirectInsert` selected. Make sure you know what you are doing!"
                );
                None
            }
        };

        let pre_generation_height = latest_block_height as usize;

        Ok(Self {
            config,
            fund_account,
            pre_generation_height,
            genesis,
            consensus_engine,
            storage,
        })
    }

    /// Deterministic, sequential generation: build and persist N blocks after latest block.
    /// Utilizes the preferred way of persistence, direct insert or consensus engine, as per config.
    /// Initializes SeedSchedule and reseeds exactly at switch heights.
    pub async fn start(mut self) -> Result<(), String> {
        info!("Starting chain generation and persistence...");

        // Number of post-genesis blocks to generate.
        let total_to_generate = self.config.chain.num_blocks.saturating_sub(1);
        if total_to_generate == 0 {
            info!("Nothing to generate (num_blocks = 1: only genesis).");
            return Ok(());
        }

        // Initialize seed schedule and the first active seed/rng.
        let schedule = SeedSchedule::new(&self.config.chain.seed);
        let mut current_seed = schedule.seed_at(1);
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

        // back the accounts&keys up
        generation_state.save_accounts(self.config.chain.output_path.clone())?;

        debug!("Building distributing block");

        // Insert block that distributes balances
        // distributing block always will be first after pre-generation state.
        let distributor_height = self.pre_generation_height + 1;
        info!(
            "Distribution block will be placed at height #{}.",
            &distributor_height
        );

        let distributor = self.build_distributing_block(&mut generation_state).await?;
        debug!("Distributor block : {:#?}", distributor);

        // Persist distribution block with a short-lived write lock

        // Persist distribution block
        // If consensus engine is configured -> call on_block (do not hold storage lock).
        // Otherwise -> direct insert into storage.
        let dist_utxo_count = distributor.data.transactions[1].data.outputs.len();
        if let Some(engine) = &mut self.consensus_engine {
            let distr_block = distributor.clone();
            match engine
                .on_block(distr_block)
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

                _ => {
                    warn!(
                        "Unexpected ConsensusVerdict for distributor block: {:?}",
                        distributor
                    );
                    return Err("Unexpected ConsensusVerdict".to_string());
                }
            }
        } else {
            // direct insert (keep short-lived lock)
            let mut storage_guard = self.storage.write().await;
            storage_guard
                .put_block(&distributor)
                .await
                .map_err(|e| format!("Cannot insert distribution block : {:?}", e))?;
        }

        // Apply the block to update state
        generation_state.apply_block(&distributor);

        info!(
            "Successfully built and persisted distributing block at height 1. It created {} outputs.",
            dist_utxo_count
        );

        /* TODO: inserting undo here if flag passed */

        // acc for tx count
        let mut tx_count = 0;

        // Main generation loop
        // TODO: Fix chaingen's 2..=total_to_generate logic, so we can generate blocks from any given height
        for height in 2..=total_to_generate {
            debug!("Building block at height {}...", height);

            // If this exact height is a switch point, reseed before building the block.
            if height != 1 && schedule.is_switch_height(height) {
                current_seed = schedule.seed_at(height);
                rng = SeedSchedule::rng_from_seed(current_seed);
                info!("Switching seed at height {} -> {}", height, current_seed);
            }

            // Helper to update tx count after block is built
            let mut update_tx_count = |data: &BlockData| {
                tx_count += data.transactions.len();
            };

            // Build block and persist it.
            let block = self
                .build_block(height, &mut rng, &mut generation_state)
                .await
                .map_err(|e| format!("failed to build block at height {}: {}", height, e))?;

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

                    _ => {
                        warn!(
                            "Unexpected ConsensusVerdict for block at height {}: {:?}",
                            height, block
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
            update_tx_count(&block.data);
            let avg_tx_count = height as f64 / tx_count as f64;

            // insert undo if needed
            // note: if config is set to PersistenceMode::ConsensusEngine, this will be done by consensus engine automatically.
            if self.config.blocks.need_undo && self.config.chain.persistence_mode == PersistenceMode::DirectInsert {
                todo!("Fix inserting BlockUndo in chaingen if `need_undo` flag provided.");
            }

            if height % 10 == 0 || height == total_to_generate {
                info!(
                    "|->  persisted {}/{} blocks (seed {}). total transactions : {}, per block(avg) : {:.2}",
                    height, total_to_generate, current_seed, tx_count, avg_tx_count
                );
            }
        }

        // log current top addresses and some stuff
        {
            let s = self.storage.read().await;
            generation_state
                .log_utxos_state(&s, false)
                .await
                .map_err(|e| e.to_string())?;
        }

        info!(
            "Done. Persisted {} blocks after genesis. With total transactions created: {}",
            total_to_generate, tx_count
        );

        // TODO: Finalize chaingen run: e.g., write summary file with some stats, etc.

        Ok(())
    }

    /// generates a deterministic and valid block at `height` that has only one transaction which evenly distributes all the balance of funding account.
    /// It uses all the existing UTXOs outputs of funder.
    pub async fn build_distributing_block(
        &self,
        state: &mut GenerationState,
    ) -> Result<Block, String> {
        // read consts from genesis.
        let consts = self
            .genesis
            .header
            .genesis_state
            .ok_or_else(|| "genesis_state is None in genesis".to_string())?
            .consensus_consts;

        // Create coinbase transaction (block subsidy for height 1)
        let miner = AccountAddress::from_hash_string(&self.config.blocks.miner_address)
            .map_err(|e| format!("invalid miner_address in config: {e}"))?;

        let subsidy = consts.block_subsidy(1);

        let coinbase_tx = Transaction::new_unsigned(TransactionData {
            version: 0,
            kind: TransactionKind::Coinbase,
            inputs: vec![],
            outputs: vec![TransactionOut {
                value: subsidy,
                recipient: miner,
            }],
        });

        // Build distribution transaction

        let dist_tx = generate_distributing_transaction(state)
            .map_err(|e| format!("Failed to generate distributing transaction: {}", e))?;

        info!(
            "Distribution transaction created with {} inputs, {} outputs, total value distributed: {}",
            dist_tx.data.inputs.len(),
            dist_tx.data.outputs.len(),
            dist_tx.data.outputs.iter().map(|o| o.value).sum::<u64>()
        );

        // Coinbase first, then distribution tx
        let mut block = Block::new(
            vec![coinbase_tx, dist_tx],
            BlockHash::empty(),
            1,
            consts.difficulty_bits_for_height(1),
            self.genesis.header.timestamp + 100,
            1,
        );

        self.mine_block_parallel_ordered(&mut block);

        Ok(block)
    }

    pub async fn build_block(
        &self,
        height: u64,
        rng: &mut ChaCha8Rng,
        state: &mut GenerationState,
    ) -> Result<Block, String> {
        if height == 0 || height == 1 {
            return Err("build_block called with height=0 (genesis)".to_string());
        }
        // Lock storage for entire block build.
        let storage_guard = self.storage.write().await;

        // Load genesis and prev block once.
        let genesis = storage_guard
            .get_block_by_height(0)
            .await
            .map_err(|e| format!("cannot read genesis: {e}"))?
            .ok_or_else(|| "genesis block is missing".to_string())?;

        let prev = storage_guard
            .get_block_by_height(height - 1)
            .await
            .map_err(|e| format!("cannot read prev block at height {}: {e}", height - 1))?
            .ok_or_else(|| format!("prev block at height {} not found", height - 1))?;

        let consts = genesis
            .header
            .genesis_state
            .ok_or_else(|| "genesis_state is None in genesis".to_string())?
            .consensus_consts;

        // Consensus fields for this height.
        let bits = consts.difficulty_bits_for_height(height);

        // Add deterministic variance to block times using the RNG
        let base_time = self.config.blocks.average_block_time_secs;
        let variance_range = base_time.min(30); // Max 30s variance or block time, whichever is smaller
        let variance = rng.random_range(0..=variance_range);
        let timestamp = prev.header.timestamp.saturating_add(base_time + variance);

        // Miner + subsidy.
        let miner = AccountAddress::from_hash_string(&self.config.blocks.miner_address)
            .map_err(|e| format!("invalid miner_address in config: {e}"))?;
        let subsidy = consts.block_subsidy(height);

        // Start with coinbase transaction
        let coinbase_tx = Transaction::new_unsigned(TransactionData {
            version: 0,
            kind: TransactionKind::Coinbase,
            inputs: vec![],
            outputs: vec![TransactionOut {
                value: subsidy,
                recipient: miner,
            }],
        });

        // Calculate how many payment transactions to generate
        let mut transactions = vec![coinbase_tx];

        // Choose accounts to use in this block

        let (min_txs, max_txs) = (
            self.config.blocks.min_transactions_per_block,
            self.config.blocks.max_transactions_per_block,
        );

        // calculate how many transactions must be in block
        let total_tx_count = rng.random_range(min_txs..=max_txs) as usize;
        let payment_tx_count = total_tx_count.saturating_sub(1); // Subtract 1 for coinbase

        if payment_tx_count == 0 {
            panic!("Payment transaction count is zero at height {}!", height);
        }

        let params = TransactionGenerationParams::default();
        let receivers_pool = state
            .get_best_receivers(state.accounts.len())
            .expect("Must be available receivers.");

        // Initialize strategy
        let mut strategy = TxGenerationStrategy::new(payment_tx_count);

        // Track UTXOs spent in this block to avoid double-spends
        let mut spent_in_block: HashSet<OutPoint> = HashSet::new();

        while strategy.should_continue() {
            strategy.record_attempt();

            // Get accounts with UTXOs
            let top_accounts =
                state.get_top_accounts_with_utxos(strategy.target_txs - strategy.successful_txs);

            for (_sender_addr, sender_key, sender_utxos) in top_accounts {
                if !strategy.should_continue() {
                    break;
                }

                // Filter out UTXOs that have already been spent in this block
                let available_utxos: IndexSet<UtxoInfo> = sender_utxos
                    .into_iter()
                    .filter(|utxo| !spent_in_block.contains(&utxo.outpoint))
                    .collect();

                // Skip if no UTXOs available
                if available_utxos.is_empty() {
                    continue;
                }

                // Use strategy to choose pattern and UTXO selection
                let pattern = strategy.choose_pattern(available_utxos.len(), &params, rng);
                let (selection_criteria, utxos_to_select) =
                    strategy.choose_utxo_strategy(pattern, available_utxos.len(), &params, rng);

                let selected_utxos = state.select_utxos_by_criteria(
                    &available_utxos,
                    selection_criteria,
                    utxos_to_select,
                );

                // Generate transaction
                if let Some(tx) = generate_transaction(
                    &sender_key,
                    pattern,
                    selected_utxos.clone(),
                    &receivers_pool,
                    rng,
                    &params,
                ) {
                    // Mark the UTXOs as spent in this block
                    for utxo in selected_utxos {
                        spent_in_block.insert(utxo.outpoint);
                    }

                    transactions.push(tx);
                    strategy.record_success();
                }
            }
        }

        debug!(
            "transaction count at the end of the block {} is : {}",
            &height,
            transactions.len()
        );

        // if only one tx in transaction (coinbase) - consider as a fail
        if transactions.len() == 1 {
            error!("Failed to build block {}.", height);
            panic!();
        }

        // Assemble block
        let mut block = Block::new(
            transactions,
            prev.block_hash(),
            height,
            bits,
            timestamp,
            1, // version
        );

        // Mine nonce in parallel using ordered scan for deterministic result.
        self.mine_block_parallel_ordered(&mut block);

        Ok(block)
    }

    /// Parallel, ordered nonce search using rayon.
    /// Scans 0..cap in increasing order; the first matching nonce is chosen (deterministic).
    fn mine_block_parallel_ordered(&self, block: &mut Block) {
        // Compute a sane attempt cap from difficulty.
        // cap ~= min(50M, 16 * 2^bits), with lower bound 1k.
        let pow_bits = block.header.difficulty_bits.min(31);
        let expected = 1u64 << pow_bits;
        let cap = expected.saturating_mul(16).clamp(1_000, 50_000_000);

        // Take a snapshot of the header without changing the shared `block` during the search.
        let base_header = block.header;

        let found = (0u32..(cap as u32)).into_par_iter().find_first(|&nonce| {
            let mut hdr = base_header;
            hdr.nonce = nonce;

            // bincode serde encode (no aliasing), hash, then difficulty check.
            let header_bytes = match bincode::serde::encode_to_vec(hdr, bincode::config::standard())
            {
                Ok(v) => v,
                Err(_) => return false,
            };
            let h = BlockHash::new(&header_bytes);
            meets_difficulty(&h, hdr.difficulty_bits)
        });

        if let Some(nonce) = found {
            block.header.nonce = nonce;
        }
    }
}
