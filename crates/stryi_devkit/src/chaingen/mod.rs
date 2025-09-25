/// Configuration struct for ChainGen.
mod config;
pub use config::*;

/// Definition and helpers for Seed struct used in ChainGen.
pub mod seed;

/// Seed schedule: mapping from height to active seed.
/// Used to create ChaCha8Rng instances for block synthesis.
mod seed_schedule;

use crate::chaingen::seed_schedule::SeedSchedule;
pub use config::*;
use rayon::prelude::{IntoParallelIterator, ParallelIterator};
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash, meets_difficulty};
use stryi_core::storage::BlockStorage;
use stryi_core::transactions::{Transaction, TransactionData, TransactionKind, TransactionOut};
use stryi_storage::{GenesisInitConfig, StorageStatus, StryiStorage};

pub struct ChainGenerator {
    /// Config for this run.
    config: ChainGenConfig,
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

        let storage_path = config.chain.output_path.clone();
        println!("Initializing storage at path: {}", storage_path.display());

        // probe storage meta information
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
        let storage = StryiStorage::initialize_in_path(storage_path, Some(genesis_config))
            .await
            .map_err(|e| e.to_string())?;

        println!("Storage initialized.");

        Ok(Self { config, storage })
    }

    /// Deterministic, sequential generation: build and persist N-1 blocks after genesis.
    /// Initializes SeedSchedule and reseeds exactly at switch heights.
    /// Note: build_block(height) is kept as-is; you can wire RNG usage inside it later.
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
        // underscore prevents unused-variable warnings until build_block uses RNG
        let mut _rng = SeedSchedule::rng_from_seed(current_seed);
        println!(
            "Seed schedule initialized. First active seed: {}",
            current_seed
        );

        for height in 1..=total_to_generate {
            // If this exact height is a switch point, reseed before building the block.
            if height != 1 && schedule.is_switch_height(height) {
                current_seed = schedule.seed_at(height);
                _rng = SeedSchedule::rng_from_seed(current_seed);
                println!("Switching seed at height {} -> {}", height, current_seed);
            }

            // Build block and persist it in order.
            let block = self
                .build_block(height)
                .await
                .map_err(|e| format!("failed to build block at height {}: {}", height, e))?;

            self.storage
                .put_block(&block)
                .await
                .map_err(|e| format!("put_block failed at height {}: {}", height, e))?;

            if height % 1000 == 0 || height == total_to_generate {
                println!(
                    "... persisted {}/{} blocks (seed {})",
                    height, total_to_generate, current_seed
                );
            }
        }

        println!(
            "Done. Persisted {} blocks after genesis.",
            total_to_generate
        );
        Ok(())
    }
    /// Build a deterministic, valid block at `height`:
    /// - loads consensus consts from genesis;
    /// - derives prev_hash/timestamp from the previous block;
    /// - creates coinbase paying subsidy(height) to configured miner;
    /// - mines nonce in parallel (ordered scan) via rayon.
    pub async fn build_block(&self, height: u64) -> Result<Block, String> {
        if height == 0 {
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
        let avg = self.config.blocks.average_block_time_secs;
        let timestamp = prev.header.timestamp.saturating_add(avg);

        // Miner + subsidy.
        let miner = AccountAddress::from_hash_string(&self.config.blocks.miner_address)
            .map_err(|e| format!("invalid miner_address in config: {e}"))?;
        let subsidy = consts.block_subsidy(height);

        // Coinbase tx: no inputs, exactly one output.
        let coinbase = Transaction::new_unsigned(TransactionData {
            version: 0,
            kind: TransactionKind::Coinbase,
            inputs: vec![],
            outputs: vec![TransactionOut {
                value: subsidy,
                recipient: miner,
            }],
        });

        // Assemble block (coinbase-only for now).
        let mut block = Block::new(
            vec![coinbase],
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
