/// Configuration struct for ChainGen.
mod config;

use crate::chaingen::state::GenerationState;
pub use config::*;
use rand::Rng;
use rand_chacha::ChaCha8Rng;
use std::cmp::min;

/// Definition and helpers for Seed struct used in ChainGen.
pub mod seed;

/// Seed schedule: mapping from height to active seed.
/// Used to create ChaCha8Rng instances for block synthesis.
mod seed_schedule;
mod state;

mod txgen;
mod utxo;

use crate::chaingen::seed_schedule::SeedSchedule;
use crate::chaingen::txgen::{
    FundAccount, TransactionGenerationParams, generate_distributing_transaction,
    generate_transaction,
};
use crate::chaingen::utxo::{UtxoInfo, UtxoSelectionCriteria, sample_transaction_pattern};
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use stryi_core::PrivateKey;
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockData, BlockHash, meets_difficulty};
use stryi_core::storage::BlockStorage;
use stryi_core::transactions::{
    OutPoint, Transaction, TransactionData, TransactionKind, TransactionOut,
};
use stryi_storage::{GenesisInitConfig, StorageStatus, StryiStorage};
use tracing::{debug, error, trace};

pub struct ChainGenerator {
    /// Config for this run.
    config: ChainGenConfig,

    /// Fund account
    pub(crate) fund_account: (AccountAddress, PrivateKey, u64, OutPoint),

    /// Cached genesis
    genesis: Block,

    /// Storage instance.
    storage: StryiStorage,
}

impl ChainGenerator {
    /// Initialize ChainGenerator with given config.
    /// This includes reading and parsing the genesis config,
    /// and initializing storage.
    pub async fn initialize(config: ChainGenConfig) -> Result<Self, String> {
        println!("Initializing ChainGenerator...");

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
        println!("Successfully read and parsed genesis config.");

        // make sure that config.funds_account exists in GenesisInitConfig and its balance != 0

        let private_key = config.clone().blocks.funding_key.into_inner();
        let funding_account = AccountAddress::from_public_key(private_key.verifying_key());
        let maybe_available_balance = genesis_config.wanted_balances.get(&funding_account);

        if let Some(balance) = maybe_available_balance {
            if *balance == 0 {
                return Err(format!(
                    "Provided `funding_key` has corresponding account in genesis, but its balance is ZERO! Account is : {}",
                    &funding_account
                ));
            }
            println!(
                "Got {} account with genesis-defined balance {}!",
                &funding_account, balance
            );
        } else {
            return Err(format!(
                "Provided `funding_key` has no corresponding account in genesis from which to distribute the balances. Provided key corresponds to {}",
                &funding_account
            ));
        }

        let available_balance =
            maybe_available_balance.expect("already checked balance is Some(_)");

        let storage_path = config.chain.output_path.clone();
        println!("Initializing storage at path: {}", storage_path.display());

        // Probe storage meta information
        let storage_status = StorageStatus::from_path(&storage_path).map_err(|e| {
            println!(
                "Failed to probe storage at {}: {e}",
                &storage_path.display()
            );
            e.to_string()
        })?;

        println!(
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

        println!("Storage initialized.");

        // read genesis
        let genesis = storage
            .get_block_by_height(0)
            .await
            .map_err(|e| format!("Unable to read genesis block from storage! Error: {e}"))?
            .ok_or("Genesis block does not exist in storage after initialization")?;

        let first_tx = genesis
            .data
            .transactions
            .first()
            .expect("Genesis transaction must exist");

        let genesis_fund_outpoint = OutPoint {
            txid: first_tx.data.hash(),
            vout: first_tx
                .data
                .outputs
                .iter()
                .position(|out| out.recipient == funding_account)
                .expect("Funding account must have a balance") as u32,
        };

        let fund_account: FundAccount = (
            funding_account,
            config.clone().blocks.funding_key,
            *available_balance,
            genesis_fund_outpoint,
        );

        debug!(?fund_account);

        Ok(Self {
            config,
            fund_account,
            genesis,
            storage,
        })
    }

    /// Deterministic, sequential generation: build and persist N-1 blocks after genesis.
    /// Initializes SeedSchedule and reseeds exactly at switch heights.
    pub async fn start(mut self) -> Result<(), String> {
        println!("Starting chain generation and persistence...");

        // Number of post-genesis blocks to generate.
        let total_to_generate = self.config.chain.num_blocks.saturating_sub(1);
        if total_to_generate == 0 {
            println!("Nothing to generate (num_blocks = 1: only genesis).");
            return Ok(());
        }

        // Initialize seed schedule and the first active seed/rng.
        let schedule = SeedSchedule::new(&self.config.chain.seed);
        let mut current_seed = schedule.seed_at(1);
        let mut rng = SeedSchedule::rng_from_seed(current_seed);

        println!(
            "Seed schedule initialized. First active seed: {}",
            current_seed
        );

        // Pre-generate all accounts deterministically using the initial seed
        // And build generation_state
        let mut generation_state = GenerationState::new_with_random_accounts(
            self.config.blocks.active_addresses_count as usize,
            self.fund_account.clone(),
            current_seed,
        );

        // print accounts
        generation_state
            .accounts
            .iter()
            .map(|acc| acc.0) // only iterate addresses
            .enumerate()
            .for_each(|(idx, address)| println!("[{}] {address}", idx + 1));

        println!(
            "Generated {} deterministic accounts",
            &generation_state.accounts.len()
        );

        // Insert block that distributes balances
        let distributor = self.build_distributing_block(&mut generation_state).await?;
        trace!(?distributor);

        self.storage
            .put_block(&distributor)
            .await
            .map_err(|e| format!("Cannot insert distribution block : {:?}", e))?;

        /* inserting undo here if flag passed */

        for height in 2..=total_to_generate {
            // If this exact height is a switch point, reseed before building the block.
            if height != 1 && schedule.is_switch_height(height) {
                current_seed = schedule.seed_at(height);
                rng = SeedSchedule::rng_from_seed(current_seed);
                println!("Switching seed at height {} -> {}", height, current_seed);
            }

            let mut tx_count = 0;
            let mut update_tx_count = |data: &BlockData| {
                tx_count += data.transactions.len();
            };

            // Build block and persist it.
            let block = self
                .build_block(height, &mut rng, &mut generation_state)
                .await
                .map_err(|e| format!("failed to build block at height {}: {}", height, e))?;

            self.storage
                .put_block(&block)
                .await
                .map_err(|e| format!("put_block failed at height {}: {}", height, e))?;

            /*            let block = self
                            .build_block(height, &mut rng, &accounts, &mut account_utxos)
                            .await
                            .map_err(|e| format!("failed to build block at height {}: {}", height, e))?;

                        self.storage
                            .put_block(&block)
                            .await
                            .map_err(|e| format!("put_block failed at height {}: {}", height, e))?;
            */

            // Update counter
            update_tx_count(&block.data);
            let avg_tx_count = height as f64 / tx_count as f64;

            // insert undo if needed
            if self.config.blocks.need_undo {
                todo!("Fix inserting BlockUndo in chaingen if `need_undo` flag provided.");
            }

            if height % 10 == 0 || height == total_to_generate {
                println!(
                    "... persisted {}/{} blocks (seed {}). total transactions : {}, per block(avg) : {}",
                    height, total_to_generate, current_seed, tx_count, avg_tx_count
                );
            }
        }

        println!(
            "Done. Persisted {} blocks after genesis.",
            total_to_generate
        );
        Ok(())
    }

    /// generates deterministic and valid block at `height` that has only one transaction which evenly distributes all the balance of funding account.
    /// this method assumes that `cfg.blocks.funding_key` corresponds to valid address that exists in genesis and has some balance (>0)
    // TODO: Make it possible to generate Distributing blocks not only in the very start of the chain, but at any given height.a
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

        // build distribution tx
        let dist_tx = generate_distributing_transaction(state).expect("cannot handle for now");

        // update state: Add UTXOs for each output of the distribution tx
        let dist_txid = dist_tx.data.hash();
        for (vout, output) in dist_tx.data.outputs.iter().enumerate() {
            let utxo = UtxoInfo {
                outpoint: OutPoint {
                    txid: dist_txid,
                    vout: vout as u32,
                },
                value: output.value,
                height_created: 1,
                is_coinbase: false,
            };
            state.add_utxo(output.recipient, utxo);
        }

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

    /// Build a deterministic, valid block at `height`:
    /// - loads consensus consts from genesis;
    /// - derives prev_hash/timestamp from the previous block;
    /// - creates coinbase paying subsidy(height) to configured miner;
    /// - creates realistic Payment transactions between active accounts using UTXOs;
    /// - mines nonce in parallel (ordered scan) via rayon.
    pub async fn build_block(
        &self,
        height: u64,
        rng: &mut ChaCha8Rng,
        state: &mut GenerationState,
    ) -> Result<Block, String> {
        if height == 0 || height == 1 {
            return Err("build_block called with height=0 (genesis)".to_string());
        }

        // Load genesis and prev block once.
        let genesis = self
            .storage
            .get_block_by_height(0)
            .await
            .map_err(|e| format!("cannot read genesis: {e}"))?
            .ok_or_else(|| "genesis block is missing".to_string())?;

        let prev = self
            .storage
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

        if payment_tx_count > 0 {
            // Get top accounts with UTXOs for transaction generation
            let top_accounts = state.get_top_accounts_with_utxos(payment_tx_count);

            // Generate payment transactions

            for (sender_addr, sender_key, sender_utxos) in
                top_accounts.into_iter().take(payment_tx_count)
            {
                // No utxo - skip tx.
                if sender_utxos.is_empty() {
                    continue;
                }

                // Generate random selection criteria for and pattern for this transaction
                let selection_criteria: UtxoSelectionCriteria = rng.random();
                let transaction_pattern = sample_transaction_pattern(rng);

                // TODO: Calculate how many UTXOs we want to spend in this tx.
                // for now use two if possible, or 1 if not.
                let _utxos_to_spend = min(sender_utxos.len(), 2);

                let selected_utxos = state.select_utxos_by_criteria(
                    &sender_utxos,
                    selection_criteria,
                    1, //utxos_to_spend,
                );

                let receivers_pool = state
                    .get_best_receivers(state.accounts.len())
                    .expect("Must be available receivers.");

                // TODO: Make params and FeePolicy configurable in transaction generator
                let params = TransactionGenerationParams::default();

                let generated_tx = generate_transaction(
                    state,
                    sender_addr,
                    &sender_key,
                    transaction_pattern,
                    selected_utxos,
                    &receivers_pool,
                    rng,
                    &params,
                    height,
                );

                if generated_tx.is_none() {
                    error!("Tried to generate TX, but got None");
                    continue;
                }

                transactions.push(generated_tx.expect("must exist"));
            }
        }
        trace!(
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
