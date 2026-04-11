//! Storage backend for blocks, UTXOs, chain state, and rollback data.
//! See `TABLES.md` for the reference for all the partitions, and storage schema

#![allow(incomplete_features)]
// This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199
#![feature(generic_const_exprs)]

mod blocks;
mod error;
mod utxo;

#[cfg(test)]
mod tests;

/// Utilities specific to `chaingen` feature.
/// It provides APIs used exclusively by the chain generation tooling (`stryi_chaingen`).
#[cfg(feature = "chaingen")]
pub mod chaingen;

mod index;
mod meta;
mod stats;
mod tx_index;
mod undo;

pub use meta::*;

use fjall::{Config as FjallConfig, PartitionCreateOptions, TxKeyspace, TxPartition};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info};

pub use crate::error::StryiStorageError;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io;
use stryi_core::address::AccountAddress;

use crate::stats::StorageStateInformation;
use stryi_core::block::{Block, BlockHash, GenesisState};
use stryi_core::storage::{BlockStorage, UtxoStorage};
use stryi_core::transactions::{OutPoint, TransactionKind, UTXO};

/// Storage handle over the Fjall keyspace and its opened partitions.
pub struct StryiStorage {
    /// Partition storing blocks keyed by block hash
    pub(crate) blocks_partition: TxPartition,

    /// Partition storing block height -> block hash
    pub(crate) heights_partition: TxPartition,

    /// Partition storing UTXOs
    pub(crate) utxo_partition: TxPartition,

    /// Partition storing address -> set of OutPoints referencing that address
    pub(crate) addresses_partition: TxPartition,

    /// Singleton chain state record.
    pub(crate) stats_partition: TxPartition,

    /// Partition storing block hash -> `stryi_core::undo::UndoData`
    pub(crate) undo_partition: TxPartition,

    /// Partition storing block hash -> `stryi_storage::index::BlockIndexData`
    pub(crate) block_index_partition: TxPartition,

    /// Partition storing transaction hash -> canonical block location metadata.
    pub(crate) transaction_index_partition: TxPartition,

    /// Keyspace for the entire database
    pub keyspace: TxKeyspace,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
// This struct stores the data needed to create a custom genesis block: balances for each address, plus block header fields.
pub struct GenesisInitConfig {
    pub wanted_balances: IndexMap<AccountAddress, u64>,
    pub genesis_state: GenesisState,
    pub version: u16,
}

#[cfg(test)]
impl GenesisInitConfig {
    /// Build a minimal genesis config for tests.
    /// NOTE: Genesis, by convention, must have at least one allocation
    pub fn new_test() -> Self {
        let mut wanted_balances = IndexMap::new();

        wanted_balances.insert(AccountAddress::new(&[0u8; 20]), 1_000_000);

        Self {
            wanted_balances,
            genesis_state: GenesisState::default(),
            version: 0,
        }
    }
}

/// Validate the storage-side invariants for a genesis block.
pub fn validate_genesis(block: &Block) -> Result<(), Box<dyn Error + Send + Sync>> {
    let h = &block.header;

    let invariant_err =
        |msg: &str| io::Error::other(format!("genesis invariant failed: {msg}")).into();

    if h.height != 0 {
        return Err(invariant_err("header.height must be 0"));
    }

    if !block.is_merkle_root_valid() {
        return Err(invariant_err("header.merkle_root must be valid"));
    }

    if h.previous_block_hash != BlockHash::empty() {
        return Err(invariant_err("header.previous_block_hash must be zero"));
    }

    if !h.is_genesis() {
        return Err(invariant_err("header.is_genesis() must be true"));
    }

    if block.data.transactions.len() != 1 {
        return Err(invariant_err("exactly one transaction is required"));
    }

    let tx = &block.data.transactions[0];

    if tx.data.kind != TransactionKind::Genesis {
        return Err(invariant_err("transaction must have genesis kind"));
    }

    if !tx.data.inputs.is_empty() {
        return Err(invariant_err("transaction must have no inputs"));
    }

    if tx.data.outputs.is_empty() {
        return Err(invariant_err(
            "genesis transaction must have at least one output",
        ));
    }

    let mut seen_addresses = HashSet::new();

    for output in &tx.data.outputs {
        if !seen_addresses.insert(output.recipient) {
            return Err(invariant_err("all allocation recipients must be unique"));
        }

        if output.value == 0 {
            return Err(invariant_err("genesis allocations must be non-zero"));
        }
    }

    Ok(())
}

/// Extract all block outputs as `(OutPoint, UTXO)` pairs.
pub fn extract_utxos_from_block(block: &Block) -> Vec<(OutPoint, UTXO)> {
    block
        .data
        .transactions
        .iter()
        .flat_map(|tx| {
            tx.data
                .outputs
                .iter()
                .enumerate()
                .map(move |(vout, output)| {
                    let outpoint = OutPoint {
                        txid: tx.data.hash(),
                        vout: vout as u32,
                    };

                    let utxo = UTXO {
                        txid: tx.data.hash(),
                        vout: vout as u32,
                        value: output.value,
                        owner: output.recipient,
                    };

                    (outpoint, utxo)
                })
        })
        .collect()
}

impl StryiStorage {
    /// Opens or creates the Fjall keyspace and all eight storage partitions.
    ///
    /// If `stats` already contains state, the existing storage is returned and
    /// `genesis_config` is ignored. On a brand-new database, this either stores
    /// genesis from `genesis_config` or initializes empty pre-genesis state.
    pub async fn initialize_in_path(
        path: PathBuf,
        genesis_config: Option<GenesisInitConfig>,
    ) -> Result<Self, StryiStorageError> {
        info!("Trying to access Stryi storage at path {}", &path.display());

        // Create or open the KeySpace
        let cfg = FjallConfig::new(path).temporary(false);
        let keyspace = cfg.open_transactional()?;

        info!("Successfully initialized key space!");
        info!(
            "Current database disk usage is : {} bytes",
            keyspace.disk_space()
        );

        // Open or create the eight partitions with default options
        let blocks_partition =
            keyspace.open_partition("blocks", PartitionCreateOptions::default())?;
        let heights_partition =
            keyspace.open_partition("heights", PartitionCreateOptions::default())?;
        let utxo_partition = keyspace.open_partition("utxo", PartitionCreateOptions::default())?;
        let addresses_partition =
            keyspace.open_partition("addresses", PartitionCreateOptions::default())?;
        let stats_partition =
            keyspace.open_partition("stats", PartitionCreateOptions::default())?;
        let undo_partition = keyspace.open_partition("undo", PartitionCreateOptions::default())?;
        let block_index_partition =
            keyspace.open_partition("block_indexes", PartitionCreateOptions::default())?;
        let transaction_index_partition =
            keyspace.open_partition("transaction_indexes", PartitionCreateOptions::default())?;

        // Create storage instance
        let mut storage = Self {
            blocks_partition,
            heights_partition,
            utxo_partition,
            addresses_partition,
            stats_partition,
            undo_partition,
            block_index_partition,
            transaction_index_partition,
            keyspace,
        };

        // Attempt to load existing chain state
        match storage.get_current_storage_state() {
            Ok(state) => {
                info!(
                    "Storage already initialized. Current chain state: latest block = {:?}, last update time = {}, blocks count = {}, chain difficulty = {}",
                    state.latest_block,
                    state.last_update_time,
                    state.blocks_count,
                    state.chain_difficulty
                );
                // Storage is already initialized; return as-is.
                return Ok(storage);
            }
            Err(StryiStorageError::NoStorageStatsFound(_)) => {
                // Database is brand new - initialize it
                debug!("Storage not initialized. Proceeding with initialization...");

                if let Some(gconfig) = genesis_config {
                    info!("Inserting genesis block (bootstrap mode)...");

                    // Initialize with genesis block - this will also create initial stats
                    storage.init_with_genesis(gconfig.clone()).await?;

                    info!(
                        "Genesis inserted: allocations={}, genesis_state={:#?}, version={}",
                        gconfig.wanted_balances.len(),
                        gconfig.genesis_state,
                        gconfig.version
                    );
                } else {
                    // Pre-Genesis: create empty layout with initial stats
                    storage.initialize_storage_state()?;
                    info!(
                        "Initialized storage in Pre-Genesis mode (no genesis block). The caller must commit genesis later."
                    );
                }
            }
            Err(e) => {
                // Some other error occurred while checking storage state.
                return Err(e);
            }
        }

        Ok(storage)
    }

    /// Inserts a genesis block if the database is empty, using the user-provided config.
    ///
    /// 1) Constructs the genesis block
    /// 2) Validates it
    /// 3) Initializes storage state
    /// 4) Calls self.put_block and self.put_utxos to store it and update the chain stats
    pub async fn init_with_genesis(
        &mut self,
        cfg: GenesisInitConfig,
    ) -> Result<(), StryiStorageError> {
        // Build the genesis block from user config
        let genesis_block = Block::new_genesis(
            cfg.version,
            HashMap::from_iter(cfg.wanted_balances),
            cfg.genesis_state,
        );

        // Validate the genesis block before storing
        validate_genesis(&genesis_block).map_err(|e| {
            StryiStorageError::InvalidGenesisBlock(format!("Genesis validation failed: {}", e))
        })?;

        self.initialize_storage_state()?;

        // Store it via put_block
        self.put_block(&genesis_block).await?;

        info!("Genesis block successfully stored.");

        // And utxos via put_utxos
        let utxos_to_insert: Vec<(OutPoint, UTXO)> = extract_utxos_from_block(&genesis_block);

        self.batch_put_utxos(utxos_to_insert).await?;

        info!("Genesis UTXOs successfully stored.");

        Ok(())
    }

    /// Creates an initial storage state if it doesn't exist.
    /// Note: This should only be called when we know state doesn't exist (after NoStorageStatsFound error).
    fn initialize_storage_state(&mut self) -> Result<(), StryiStorageError> {
        // Create an initial state
        let initial_state = StorageStateInformation {
            latest_block: (0, BlockHash::empty()), // Empty is basically genesis block
            last_update_time: 0,
            blocks_count: 0,
            chain_difficulty: 0,
        };

        // Store initial state
        self.update_storage_state(initial_state)
    }

    /// Durably persists all committed data to disk.
    /// Should be called during graceful shutdown to avoid data loss.
    pub fn persist(&self) -> Result<(), StryiStorageError> {
        self.keyspace
            .persist(fjall::PersistMode::SyncAll)
            .map_err(StryiStorageError::FjallError)
    }
}
