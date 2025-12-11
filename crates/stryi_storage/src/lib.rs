//! The database uses a **key-value storage model** to ensure efficiency and scalability.
//!
//! We maintain **seven** separate partitions in this design:
//!
//! 1. **Blocks**
//!    - Key   : `stryi_core::block::BlockHash` (32 bytes of the block hash)
//!    - Value : A `bincode`-serialized `stryi_core::block::Block`
//!    
//!    This partition stores the full blocks in the blockchain. Each block references the previous one via
//!    its header, and we also track the block's height separately in another partition.
//!
//! 2. **Height**
//!    - Key   : 8 bytes of `height` (big-endian u64)
//!    - Value : 32 bytes of block hash (the same as in the blocks partition key)
//!
//!    This partition maps each block's height to its `BlockHash`, enabling quick lookups by height. It
//!    also helps us retrieve the chain in sequence or find the latest block via `last_key_value()`.
//!
//! 3. **UTXO**
//!    - Key   : 36 bytes `[txid (32 bytes) | vout (4 bytes, big-endian)]`
//!    - Value : A `bincode`-serialized `UTXO` (unspent output)
//!
//!    This partition stores unspent transaction outputs (UTXOs). The key is the combination of a
//!    transaction hash (32 bytes) and an output index `vout` (4 bytes). Each entry's value is the UTXO
//!    data (including its owner address, value, etc.).
//!
//! 4. **Addresses**
//!    - Key   : 20 bytes of `AccountAddress`
//!    - Value : A `bincode`-serialized `HashSet<OutPoint>` referencing all outpoints belonging to that address
//!
//!    This partition is our **address index**, mapping each address to the set of outpoints owned by
//!    that address. When inserting or removing UTXOs, we keep this index in sync. Then, for lookups such
//!    as `get_utxos_for_address`, we can quickly retrieve the relevant outpoints without scanning all
//!    UTXOs.
//! 5. **Stats**
//!     - Key  : 32 zero bytes
//!     - Value : A `bincode`-serialized `StorageStateInformation`.
//!
//!     The only goal of this partition is to hold current information about storage state. We will
//!    update stats after each new block. This allows us to perform some consensus-related logic of comparing different chains.
//! 6. **Undo**
//!     - Key : `stryi_core::block::BlockHash` (32 bytes of the block hash)
//!     - Value : A `bincode`-serialized `stryi_core::BlockUndo` object
//!
//!     This partition is our per-block backup data. The thing allows us easily restore pre-block state, by just keeping
//!    `BlockUndo` in base. Restoration is just simple as deleting all the new outputs and restoring all the existing ones.
//!    High-level struct for implementing this functionality is `ChainReorganizer`
//!
//! 7. **Block Indexes**
//!     - Key : `stryi_core::block::BlockHash` (32 bytes of the block hash)
//!     - Value : A `bincode`-serialized `stryi_storage::index::BlockIndexData` object
//!
//! By maintaining these 7 partitions, we get efficient lookups for blocks, block heights, UTXOs by
//! outpoint, addresses to outpoint sets and will be able to correctly and safely reorganize chain for consensus purposes.

#![allow(incomplete_features)]
// This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199
#![feature(generic_const_exprs)]
#![feature(new_range_api)]

mod blocks;
mod error;
mod utxo;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::io;

/// Utilities specific to `chaingen` feature.
/// It provides APIs used exclusively by the chain generation tooling (`stryi_chaingen`).
#[cfg(feature = "chaingen")]
pub mod chaingen;

mod index;
mod meta;
mod stats;
mod undo;

pub use meta::*;

use fjall::{Config as FjallConfig, PartitionCreateOptions, TxKeyspace, TxPartition};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info};

pub use crate::error::StryiStorageError;
use stryi_core::address::AccountAddress;

use crate::stats::StorageStateInformation;
use stryi_core::block::{Block, BlockHash, GenesisState};
use stryi_core::storage::{BlockStorage, UtxoStorage};
use stryi_core::transactions::{OutPoint, TransactionKind, UTXO};

/// `StryiStorage` manages seven partitions within a single Fjall keyspace:
/// - `blocks_partition`: For storing blocks keyed by hash
/// - `heights_partition`: For storing mappings from height -> hash
/// - `utxo_partition`: For storing actual UTXOs keyed by outpoints (txid+vout)
/// - `addresses_partition`: For mapping addresses -> set of outpoints
/// - `stats_partition`: For storing the only value with current statistics for entire chain
/// - `undo_partition` : For storing per-block restoration data to be able to restore any previous state
/// - `block_index_partition`: For storing some metadata like parent_hash, height, current chain work, etc
///
/// Each partition is opened once at initialization, and we keep a reference in this struct.
pub struct StryiStorage {
    /// Partition storing blocks keyed by block hash
    pub(crate) blocks_partition: TxPartition,

    /// Partition storing block height -> block hash
    pub(crate) heights_partition: TxPartition,

    /// Partition storing UTXOs
    pub(crate) utxo_partition: TxPartition,

    /// Partition storing address -> set of OutPoints referencing that address
    pub(crate) addresses_partition: TxPartition,

    /// Partition stores only one value - current chain state, must be updated after each new block or a reorganization
    pub(crate) stats_partition: TxPartition,

    /// Partition storing block hash -> `stryi_core::undo::UndoData`
    pub(crate) undo_partition: TxPartition,

    /// Partition storing block hash -> `stryi_storage::index::BlockIndexData`
    pub(crate) block_index_partition: TxPartition,

    /// Keyspace for the entire database
    pub keyspace: TxKeyspace,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
// This struct stores the data needed to create a custom genesis block: balances for each address, plus block header fields.
pub struct GenesisInitConfig {
    pub wanted_balances: HashMap<AccountAddress, u64>,
    pub genesis_state: GenesisState,
    pub version: u16,
}

#[cfg(test)]
impl GenesisInitConfig {
    /// Creates new GenesisBlockConfig with some reasonable parameters for tests
    /// NOTE: Genesis, by convention, must have at least one allocation
    pub fn new_test() -> Self {
        let mut wanted_balances = HashMap::new();

        wanted_balances.insert(AccountAddress::new(&[0u8; 20]), 1_000_000);

        Self {
            wanted_balances,
            genesis_state: GenesisState::default(), // Use default for testing.
            version: 0,
        }
    }
}

/// Strict header/body invariants for a genesis candidate.
pub fn validate_genesis(block: &Block) -> Result<(), Box<dyn Error + Send + Sync>> {
    let h = &block.header;

    // simple helper for errors
    let invariant_err =
        |msg: &str| io::Error::other(format!("genesis invariant failed: {msg}")).into();

    // The genesis block must be height 0.
    if h.height != 0 {
        return Err(invariant_err("header.height must be 0"));
    }

    // Merkle root must be valid
    if !block.is_merkle_root_valid() {
        return Err(invariant_err("header.merkle_root must be valid"));
    }

    // The previous hash must be all zeros for the root.
    if h.previous_block_hash != BlockHash::empty() {
        return Err(invariant_err("header.previous_block_hash must be zero"));
    }

    // Must have is_genesis=true
    if !h.is_genesis() {
        return Err(invariant_err("header.is_genesis() must be true"));
    }

    // Exactly one transaction
    if block.data.transactions.len() != 1 {
        return Err(invariant_err("exactly one transaction is required"));
    }

    let tx = &block.data.transactions[0];

    // The only transaction shall be Genesis kind
    if tx.data.kind != TransactionKind::Genesis {
        return Err(invariant_err("transaction must have genesis kind"));
    }

    // .. and have no inputs
    if !tx.data.inputs.is_empty() {
        return Err(invariant_err("transaction must have no inputs"));
    }

    // Genesis transaction must have at least one output
    if tx.data.outputs.is_empty() {
        return Err(invariant_err(
            "genesis transaction must have at least one output",
        ));
    }

    // All the recipients of allocations must be unique (no double funding for a single account)
    let mut seen_addresses = HashSet::new();

    for output in &tx.data.outputs {
        if !seen_addresses.insert(output.recipient) {
            return Err(invariant_err("all allocation recipients must be unique"));
        }

        // Check for zero-value allocations
        if output.value == 0 {
            return Err(invariant_err("genesis allocations must be non-zero"));
        }
    }

    // Is that all the checks we need for genesis?

    Ok(())
}

/// simple helper function to extract all UTXOs from a block
/// returns a vector of (OutPoint, UTXO) tuples
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
    /// Creates (or opens) the database at the given `path`.
    ///
    /// Behavior:
    /// - Always opens/creates the Fjall keyspace and the seven partitions (blocks, heights, utxo,
    ///   addresses, stats, undo, block_indexes) in transactional mode.
    /// - If the `stats` partition already contains state, the storage is treated as initialized
    ///   and returned as-is (the `genesis_config` argument is ignored).
    /// - If no state is found (brand-new database):
    ///     - When `genesis_config` is Some(..): build and insert the genesis block, initialize stats, return the handle.
    ///     - When `genesis_config` is None: create an empty layout with initial stats and return the handle
    ///       without inserting a genesis block; the caller may commit genesis later (e.g., after network sync).
    ///
    /// Notes:
    /// - This function performs no network I/O.
    /// - Callers that defer genesis should ensure it is committed before exposing chain-dependent services.
    /// - Acquires an exclusive lock file to prevent concurrent initialization attempts.
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

        // Open or create the seven partitions with default options
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

        // Create storage instance
        let mut storage = Self {
            blocks_partition,
            heights_partition,
            utxo_partition,
            addresses_partition,
            stats_partition,
            undo_partition,
            block_index_partition,
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
                        "Genesis inserted: {} allocations, genesis's chain statics = {:?}, version={}",
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
        let genesis_block = Block::new_genesis(cfg.version, cfg.wanted_balances, cfg.genesis_state);

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

    /// Creates initial storage state if it doesn't exist.
    /// Note: This should only be called when we know state doesn't exist (after NoStorageStatsFound error).
    fn initialize_storage_state(&mut self) -> Result<(), StryiStorageError> {
        // Create initial state
        let initial_state = StorageStateInformation {
            latest_block: (0, BlockHash::empty()), // Empty is basically genesis block
            last_update_time: 0,
            blocks_count: 0,
            chain_difficulty: 0,
        };

        // Store initial state
        self.update_storage_state(initial_state)
    }
}
