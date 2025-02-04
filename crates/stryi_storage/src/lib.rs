//! The database uses a **key-value storage model** to ensure efficiency and scalability.
//! After quite a bit of research, I've decided that we are using a KV database with 3 separate tables.
//! I think this is one of the best solutions for *StryiChain* so far.
//!
//!
//! ## Tables
//! ### 1. Blocks
//! ```plaintext
//! Key   : [`stryi_core::block::BlockHash`]
//! Value : Serialized (via bincode) [`stryi_core::block::Block`]
//!
//!     What:
//!     Contains all blocks in the blockchain. Each block references the previous one.
//!     Why:
//!     Maintains the blockchain structure and enables sequential block retrieval.
//!
//! 2. Height
//!
//! Key   : unsigned int (height)
//! Value : [`stryi_core::block::BlockHash`]
//!
//!     What:
//!     Maps each block’s height to its BlockHash.
//!     Why:
//!     Enables quick block lookups by height for navigation and validation.
//!
//! 3. UTXO (Unspent Transaction Output)
//!
//! Key   : tx_hash:index
//! Value : bincoded UtxoOutput
//!
//!     What:
//!     Stores unspent transaction outputs (UTXOs), with the key as a combination of transaction hash and index.
//!     Why:
//!     Tracks spendable funds and ensures transaction validation.

#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // I don`t even use this thing directly in my project, but adding it here is a way to avoid https://github.com/rust-lang/rust/issues/133199

mod error;
mod blocks;

use std::path::PathBuf;
use fjall::{Config as FjallConfig, PartitionCreateOptions, Slice, TxKeyspace, TxPartition};
use tracing::info;

pub use crate::error::StryiStorageError;
use stryi_core::block::{Block, BlockHash};
use stryi_core::transactions::{OutPoint, UTXO};

/// The database uses a key-value storage model with 3 separate partitions:
///   1. Blocks      (BlockHash -> bincode(Block))
///   2. Height      (height u64 -> BlockHash)
///   3. UTXO        (String "tx_hash:index" -> bincode(UTXO))
pub struct StryiStorage {
    /// Partition storing blocks keyed by stringified BlockHash
    blocks_partition: TxPartition,

    /// Partition mapping a height (u64) to a BlockHash
    heights_partition: TxPartition,

    /// Partition storing unspent transaction outputs
    utxo_partition: TxPartition,
    
    /// Keyspace value for entire database 
    keyspace:  TxKeyspace

}

impl StryiStorage {
    /// Creates (or opens) the database at the given `path`, and sets up three partitions:
    ///   "blocks", "height", "utxo"
    pub fn initialize_in_path(path: PathBuf) -> Result<Self, StryiStorageError> {
        info!("Trying to access Stryi storage at path {}", &path.display());

        // Create or open the KeySpace
        let cfg = FjallConfig::new(path)
            .temporary(false);

        // Use transactional mode
        let keyspace = cfg.open_transactional()?;

        info!("Successfully initialized key space!");
        info!("Current database disk usage is : {} bytes", keyspace.disk_space());

        // Open or create the three partitions with default options
        let blocks_partition = keyspace.open_partition("blocks", PartitionCreateOptions::default())?;
        let heights_partition = keyspace.open_partition("heights", PartitionCreateOptions::default())?;
        let utxo_partition = keyspace.open_partition("utxo", PartitionCreateOptions::default())?;

        // Now we can build our storage struct
        Ok(StryiStorage {
            blocks_partition,
            heights_partition,
            utxo_partition,
            keyspace
        })
    }
}