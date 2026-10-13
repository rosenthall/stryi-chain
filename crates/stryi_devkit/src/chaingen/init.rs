use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info};

use crate::chaingen::funding_account::FundAccount;
use crate::chaingen::{ChainGenConfig, ChainGenerator, check_generation_deadline};

use stryi_core::PrivateKey;
use stryi_core::block::Block;
use stryi_core::consensus::{BlockValidator, ConsensusConsts, StryiConsensusEngine};
use stryi_core::difficulty::DifficultyCalc;
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use stryi_core::transactions::{OutPoint, TransactionKind, UtxoProcessor};
use stryi_storage::{GenesisInitConfig, StorageStatus, StryiStorage};

impl ChainGenerator {
    /// Initialize ChainGenerator.
    ///
    /// Semantics:
    /// - Storage metadata (`StorageStatus`) is NOT treated as authoritative chain state.
    /// - Actual chain state is derived from the current tip in storage.
    /// - Genesis UTXOs are validated ONLY if chain height == 0.
    /// - Safe to run multiple times against the same storage (resume-friendly).
    pub async fn initialize(config: ChainGenConfig) -> Result<Self, String> {
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

        // open or initialize storage, then wrap in Arc<RwLock<_>> for shared usage
        let storage = Arc::new(RwLock::new(
            open_storage(&config.chain.output_path, &genesis_config).await?,
        ));

        check_generation_deadline(generation_deadline, None)?;
        info!("Storage initialized.");

        let (chain_height, genesis, consensus_consts) = read_chain_state(&storage).await?;
        check_generation_deadline(generation_deadline, Some(chain_height))?;
        debug!("Chain height from tip() is {chain_height}");
        validate_genesis_utxos_if_needed(&storage, &genesis, chain_height, generation_deadline)
            .await?;
        check_generation_deadline(generation_deadline, Some(chain_height))?;

        // Genesis pow params are the single source of truth for the difficulty
        // calc; building it here also validates the params before generation starts.
        let difficulty_calc = DifficultyCalc::new(consensus_consts.pow)
            .map_err(|e| format!("Invalid pow params in genesis: {e}"))?;

        info!("Checking funding account.");
        let fund_account =
            load_funding_account(storage.clone(), config.blocks.funding_key.clone()).await?;

        check_generation_deadline(generation_deadline, Some(chain_height))?;

        // initialize consensus engine
        let consensus_engine =
            init_consensus_engine(storage.clone(), consensus_consts, difficulty_calc).await?;
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

async fn open_storage(
    path: &Path,
    genesis_config: &GenesisInitConfig,
) -> Result<StryiStorage, String> {
    let status = StorageStatus::from_path(path)
        .map_err(|e| format!("Failed to probe storage {}: {e}", path.display()))?;

    info!("Storage status at {} => {:?}", path.display(), status);

    match status {
        StorageStatus::NoGenesis => {
            info!(
                "Storage metadata missing (NoGenesis). Initializing or opening with genesis config."
            );
            StryiStorage::initialize_in_path(path.to_path_buf(), Some(genesis_config.clone()))
                .await
                .map_err(|e| format!("Failed to initialize storage: {e}"))
        }

        StorageStatus::Initialized { .. } => {
            info!("Storage metadata present. Opening existing storage.");
            StryiStorage::initialize_in_path(path.to_path_buf(), None)
                .await
                .map_err(|e| format!("Failed to open existing storage: {e}"))
        }

        StorageStatus::Corrupted { reason } => {
            Err(format!("Storage metadata is corrupted: {reason}"))
        }
    }
}

async fn read_chain_state(
    storage: &Arc<RwLock<StryiStorage>>,
) -> Result<(u64, Block, ConsensusConsts), String> {
    // read tip under a short read lock
    let (height, hash) = {
        let s = storage.read().await;
        s.tip()
            .await
            .map_err(|e| format!("Unable to get chain tip: {e}"))?
    };

    info!(
        "Current chain state: latest block = ({}, {:?})",
        height, hash
    );

    // fetch genesis under read lock
    let genesis = {
        let s = storage.read().await;
        s.get_block_by_height(0)
            .await
            .map_err(|e| format!("Failed to read genesis block: {e}"))?
            .ok_or("Genesis block not found in storage")?
    };

    let consensus_consts = genesis
        .header
        .genesis_state
        .as_ref()
        .ok_or("genesis_state is None in genesis block")?
        .consensus_consts;

    Ok((height, genesis, consensus_consts))
}

async fn validate_genesis_utxos_if_needed(
    storage: &Arc<RwLock<StryiStorage>>,
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

    // hold read lock while we query utxos (short-lived)
    let s = storage.read().await;

    for (vout, _) in genesis_tx.data.outputs.iter().enumerate() {
        check_generation_deadline(generation_deadline, Some(chain_height))?;
        let outpoint = OutPoint {
            txid,
            vout: vout as u32,
        };

        let utxo = s
            .get_utxo(outpoint)
            .await
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

async fn load_funding_account(
    storage: Arc<RwLock<StryiStorage>>,
    private_key: PrivateKey,
) -> Result<FundAccount, String> {
    // Create a short-lived lock
    let s = storage.read().await;
    let funder = FundAccount::load_from_private_key(private_key, &s)
        .await
        .map_err(|e| format!("Failed to load funding account: {e}"))?;

    info!(
        "Fund account does exist in chain, and has balance ({})",
        funder.total_balance()
    );

    Ok(funder)
}
async fn init_consensus_engine(
    storage: Arc<RwLock<StryiStorage>>,
    consts: ConsensusConsts,
    difficulty_calc: DifficultyCalc,
) -> Result<StryiConsensusEngine<StryiStorage>, String> {
    let validator = BlockValidator::new(consts, difficulty_calc);
    let utxo_processor = UtxoProcessor::new();
    let engine = StryiConsensusEngine::new(consts, validator, utxo_processor, storage)
        .await
        .map_err(|e| format!("Failed to initialize consensus engine: {e:?}"))?;
    engine.startup_message();
    Ok(engine)
}
