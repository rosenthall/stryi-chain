use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, info};

use crate::chaingen::funding_account::FundAccount;
use crate::chaingen::{ChainGenConfig, ChainGenerator, check_generation_deadline};

use stryi_core::PrivateKey;
use stryi_core::block::Block;
use stryi_core::consensus::{BlockValidator, ConsensusConsts, StryiConsensusEngine};
use stryi_core::difficulty::DifficultyCalc;
use stryi_core::storage::{
    BlockStorage, ChainMetadata, ChainTransition, FullNodeStorage, StorageReader, StorageStats,
    UtxoStorage,
};
use stryi_core::transactions::{OutPoint, TransactionKind, UtxoProcessor};
use stryi_storage::{GenesisInitConfig, STORAGE_SCHEMA_VERSION, StryiStorage};

impl ChainGenerator {
    /// Initialize ChainGenerator.
    ///
    /// Semantics:
    /// - Height comes from the stored tip, consensus constants from the saved genesis.
    /// - Genesis UTXOs are validated ONLY if chain height == 0.
    /// - Safe to run multiple times against the same storage (resume-friendly).
    pub fn initialize(config: ChainGenConfig) -> Result<Self, String> {
        let generation_deadline = config
            .chain
            .max_generation_time_secs
            .map(|seconds| {
                Instant::now()
                    .checked_add(Duration::from_secs(seconds))
                    .ok_or_else(|| {
                        "max_generation_time_secs exceeds the monotonic clock range".to_string()
                    })
            })
            .transpose()?;

        check_generation_deadline(generation_deadline, None)?;
        info!("Initializing ChainGenerator...");

        std::fs::create_dir_all(&config.chain.output_path)
            .map_err(|e| format!("Failed to create output directory: {e}"))?;

        check_generation_deadline(generation_deadline, None)?;
        let genesis_config = read_genesis_config(&config.chain.genesis_path)?;
        check_generation_deadline(generation_deadline, None)?;

        info!("Successfully read and parsed genesis config.");

        let storage = open_storage(&config, &genesis_config)?;

        check_generation_deadline(generation_deadline, None)?;
        info!("Storage initialized.");

        let (chain_height, genesis, consensus_consts) = read_chain_state(&storage)?;
        check_generation_deadline(generation_deadline, Some(chain_height))?;
        debug!("Chain height from tip() is {chain_height}");

        validate_genesis_utxos_if_needed(&storage, &genesis, chain_height, generation_deadline)?;
        check_generation_deadline(generation_deadline, Some(chain_height))?;

        // Genesis pow params are the single source of truth for the difficulty
        // calc; building it here also validates the params before generation starts.
        let difficulty_calc = DifficultyCalc::new(consensus_consts.pow)
            .map_err(|e| format!("Invalid pow params in genesis: {e}"))?;

        info!("Checking funding account.");
        let fund_account =
            load_funding_account(storage.clone(), config.blocks.funding_key.clone())?;

        check_generation_deadline(generation_deadline, Some(chain_height))?;

        // initialize consensus engine
        let consensus_engine =
            init_consensus_engine(storage.clone(), consensus_consts, difficulty_calc)?;
        check_generation_deadline(generation_deadline, Some(chain_height))?;

        Ok(Self {
            config,
            generation_deadline,
            fund_account,
            pre_generation_height: chain_height,
            consensus_consts,
            difficulty_calc,
            consensus_engine,
            storage,
        })
    }
}

fn read_genesis_config(path: &Path) -> Result<GenesisInitConfig, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read genesis config {}: {e}", path.display()))?;

    serde_json::from_str(&content).map_err(|e| {
        format!(
            "Failed to parse genesis config at {}: {}",
            path.display(),
            e
        )
    })
}

fn open_storage(
    config: &ChainGenConfig,
    genesis_config: &GenesisInitConfig,
) -> Result<StryiStorage, String> {
    let genesis = Block::new_genesis(
        genesis_config.version,
        HashMap::from_iter(genesis_config.wanted_balances.clone()),
        genesis_config.genesis_state,
    );

    stryi_core::storage::validate_genesis(&genesis).map_err(|e| e.to_string())?;

    let storage = StryiStorage::initialize_in_path(config.chain.output_path.clone(), None)
        .map_err(|e| format!("Failed to initialize storage: {e}"))?;
    let metadata = genesis_metadata(config)?;

    storage
        .apply_transition(ChainTransition::Genesis {
            block: &genesis,
            metadata: &metadata,
        })
        .map_err(|e| format!("Failed to initialize genesis: {e}"))?;

    Ok(storage)
}

fn genesis_metadata(config: &ChainGenConfig) -> Result<ChainMetadata, String> {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("cannot read local clock: {e}"))?
        .as_secs();
    let created_unix_ts =
        i64::try_from(now_secs).map_err(|_| "local clock exceeds metadata range")?;

    // The genesis transition keeps the original creation time on reopen.
    Ok(ChainMetadata {
        schema_version: STORAGE_SCHEMA_VERSION,
        chain_name: config
            .chain
            .chain_name
            .clone()
            .unwrap_or_else(|| "dev".to_string()),
        protocol_version: config.chain.protocol_version.unwrap_or(1),
        created_unix_ts,
    })
}

fn read_chain_state(storage: &StryiStorage) -> Result<(u64, Block, ConsensusConsts), String> {
    let view = storage.read().map_err(|e| e.to_string())?;
    let (height, hash) = view
        .tip()
        .map_err(|e| format!("Unable to get chain tip: {e}"))?;

    let genesis = view
        .get_block_by_height(0)
        .map_err(|e| format!("Failed to read genesis block: {e}"))?
        .ok_or("Genesis block not found in storage")?;

    info!("Current chain state: latest block = ({height}, {hash:?})");

    let consensus_consts = genesis
        .header
        .genesis_state
        .as_ref()
        .ok_or("genesis_state is None in genesis block")?
        .consensus_consts;

    Ok((height, genesis, consensus_consts))
}

fn validate_genesis_utxos_if_needed(
    storage: &StryiStorage,
    genesis: &Block,
    chain_height: u64,
    generation_deadline: Option<Instant>,
) -> Result<(), String> {
    if chain_height != 0 {
        info!(
            "Chain height is {}. Skipping genesis UTXO validation.",
            chain_height
        );

        return Ok(());
    }

    info!("Chain tip at height 0. Validating genesis UTXO set.");

    let genesis_tx = genesis
        .data
        .transactions
        .first()
        .filter(|tx| tx.data.kind == TransactionKind::Genesis)
        .ok_or("Genesis block's first transaction must be of kind Genesis")?;

    let txid = genesis_tx.data.hash();

    let s = storage.read().map_err(|e| e.to_string())?;

    for (vout, _) in genesis_tx.data.outputs.iter().enumerate() {
        check_generation_deadline(generation_deadline, Some(chain_height))?;

        let outpoint = OutPoint {
            txid,
            vout: vout as u32,
        };

        let utxo = s
            .get_utxo(outpoint)
            .map_err(|e| format!("Failed to get UTXO from storage: {e}"))?;

        if utxo.is_none() {
            return Err(format!(
                "Genesis UTXO not found in storage for OutPoint: {:?}",
                outpoint
            ));
        }
    }

    Ok(())
}

fn load_funding_account(
    storage: StryiStorage,
    private_key: PrivateKey,
) -> Result<FundAccount, String> {
    let funder = FundAccount::load_from_private_key(private_key, &storage)
        .map_err(|e| format!("Failed to load funding account: {e}"))?;

    info!(
        "Fund account does exist in chain, and has balance ({})",
        funder.total_balance()
    );

    Ok(funder)
}

fn init_consensus_engine(
    storage: StryiStorage,
    consts: ConsensusConsts,
    difficulty_calc: DifficultyCalc,
) -> Result<StryiConsensusEngine<StryiStorage>, String> {
    let validator = BlockValidator::new(consts, difficulty_calc);
    let utxo_processor = UtxoProcessor::new();

    let engine = StryiConsensusEngine::new(consts, validator, utxo_processor, Arc::new(storage))
        .map_err(|e| format!("Failed to initialize consensus engine: {e:?}"))?;

    engine.startup_message();

    Ok(engine)
}
