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
//!     update stats after each new block. This allows us to perform some consensus-related logic of comparing different chains.
//! 6. **Undo**
//!     - Key : `stryi_core::block::BlockHash` (32 bytes of the block hash)
//!     - Value : A `bincode`-serialized `stryi_core::BlockUndo` object
//!     
//!     This partition is our per-block backup data. The thing allows us easily restore pre-block state, by just keeping 
//!     `BlockUndo` in base. Restoration is just simple as deleting all the new outputs and restoring all the existing ones. 
//!     High-level struct for implementing this functionality is `ChainReorganizer`
//!
//! 7. **Block Indexes**
//!     - Key : `stryi_core::block::BlockHash` (32 bytes of the block hash)
//!     - Value : A `bincode`-serialized `stryi_storage::index::BlockIndexData` object 
//! 
//! By maintaining these 7 partitions, we get efficient lookups for blocks, block heights, UTXOs by
//! outpoint, addresses to outpoint sets and will be able to correctly and safely reorganize chain for consensus purposes.

#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199

mod error;
mod blocks;
mod utxo;


#[cfg(test)]
mod tests;

/// Reorganizer module provides a convenient interface for performing chain reorganization:
///
/// * It defines a `ChainReorganizer` structure that locates the common ancestor between two tips
///   and rolls back the old chain, then applies and validates blocks for the new chain.
/// * This process ensures atomic reorg under a single storage write lock.
/// * Used primarily when a fork becomes heavier than the current best chain.
mod reorganizer;

use std::collections::{HashMap, HashSet};
pub use reorganizer::*;

mod stats;
mod undo;
mod index;

use std::path::PathBuf;
use fjall::{Config as FjallConfig, PartitionCreateOptions, TxKeyspace, TxPartition};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

pub use crate::error::StryiStorageError;
pub use blocks::*;
use stryi_core::address::AccountAddress;
pub use utxo::*;

use stryi_core::block::{Block, BlockHash};
use stryi_core::storage::BlockStorage;
use crate::stats::StorageStateInformation;

/// `StryiStorage` manages seven partitions within a single Fjall keyspace:
/// - `blocks_partition`: For storing blocks keyed by hash
/// - `heights_partition`: For storing mappings from height → hash
/// - `utxo_partition`: For storing actual UTXOs keyed by (txid+vout)
/// - `addresses_partition`: For mapping addresses → set of outpoints
/// - `stats_partition`: For storing the only value with current statistics for entire chain
/// - `undo_partition` : For storing per-block restoration data to be able to restore any previous state
/// - `block_index_partition`: For storing some metadata like parent_hash, height, current chain work, etc
///
/// Each partition is opened once at initialization, and we keep a reference in this struct.
pub struct StryiStorage {
    /// Partition storing blocks keyed by block hash
    pub(crate) blocks_partition: TxPartition,

    /// Partition storing block height → block hash
    pub(crate) heights_partition: TxPartition,

    /// Partition storing UTXOs
    pub(crate) utxo_partition: TxPartition,

    /// Partition storing address → set of OutPoints referencing that address
    pub(crate) addresses_partition: TxPartition,
    
    /// Partition stores only one value - current chain state, must be updated after each new block or a reorganization
    pub(crate) stats_partition: TxPartition,

    /// Partition storing block hash → `stryi_core::undo::UndoData` 
    pub(crate) undo_partition: TxPartition,
    
    /// Partition storing block hash → `stryi_storage::index::BlockIndexData`
    pub(crate) block_index_partition: TxPartition,
    
    /// Keyspace for the entire database
    pub keyspace: TxKeyspace,
}



#[derive(Clone, Debug, Serialize, Deserialize)]
// This struct stores the data needed to create a custom genesis block: balances for each address, plus block header fields.
pub struct GenesisInitConfig {
    pub wanted_balances: HashMap<AccountAddress, u64>,
    pub difficulty_bits: u8,
    pub version: u16,
}



impl StryiStorage {
    /// Creates (or opens) the database at the given `path`.
    ///
    /// If path database doesn't exist yet - it must be initialized with `genesis_config`
    /// If database is not initialized and no initialization config is provided - returns `NoInitializationConfigProvided`
    ///
    /// We open it in transactional mode so we can do atomic writes across multiple partitions.
    pub async fn initialize_in_path(path: PathBuf, genesis_config: Option<GenesisInitConfig>) -> Result<Self, StryiStorageError> {
        info!("Trying to access Stryi storage at path {}", &path.display());

        // Create or open the KeySpace
        let cfg = FjallConfig::new(path).temporary(false);
        let keyspace = cfg.open_transactional()?;

        info!("Successfully initialized key space!");
        info!("Current database disk usage is : {} bytes", keyspace.disk_space());

        // Open or create the seven partitions with default options
        let blocks_partition = keyspace.open_partition("blocks", PartitionCreateOptions::default())?;
        let heights_partition = keyspace.open_partition("heights", PartitionCreateOptions::default())?;
        let utxo_partition = keyspace.open_partition("utxo", PartitionCreateOptions::default())?;
        let addresses_partition = keyspace.open_partition("addresses", PartitionCreateOptions::default())?;
        let stats_partition = keyspace.open_partition("stats", PartitionCreateOptions::default())?;
        let undo_partition = keyspace.open_partition("undo", PartitionCreateOptions::default())?;
        let block_index_partition = keyspace.open_partition("block_indexes", PartitionCreateOptions::default())?;


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

        if let Err(StryiStorageError::NoStorageStatsFound(_)) = storage.get_current_storage_state() {

            // Initialize very first state
            storage.initialize_storage_state()?;


            // Insert genesis block if genesis_config was provided
            if let Some(gconfig) = genesis_config {
                info!("Inserting genesis block!");
                storage.init_with_genesis(gconfig.clone()).await?;
                info!("Successfully inserted genesis block with {} predefined balances, {} basic difficulty bits and version {}", &gconfig.wanted_balances.len(), &gconfig.difficulty_bits, &gconfig.version);
            } else {
                // if user didn't provide config, but the DB is brand new - return error
                error!("Database is not initialized and no genesis config provided!");
                return Err(StryiStorageError::NoInitializationConfigProvided);
            }
        }

        Ok(storage)
    }


    /// Inserts a genesis block if the database is empty, using the user-provided config.
    ///
    /// 1) Constructs the genesis block
    /// 2) Calls self.put_block to store it and update the chain stats
    pub async fn init_with_genesis(
        &mut self,
        cfg: GenesisInitConfig
    ) -> Result<(), StryiStorageError> {

        // Build the genesis block from user config
        let genesis_block = Block::new_genesis(
            cfg.version,
            cfg.difficulty_bits,
            cfg.wanted_balances,
        );

        // try store it via put_block
        self.put_block(&genesis_block).await
    }

    /// Creates initial storage state if it doesn't exist
    fn initialize_storage_state(&mut self) -> Result<(), StryiStorageError> {
        // Check if state already exists
        if self.get_current_storage_state().is_ok() {
            return Ok(());
        }

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
