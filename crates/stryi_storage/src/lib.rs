//! The database uses a **key-value storage model** to ensure efficiency and scalability.
//!
//! We maintain **four** separate partitions in this design:
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
//!    - Key   : 20 bytes of `AccountAddress` (assuming `AddressHasher::SIZE = 20`)
//!    - Value : A `bincode`-serialized collection (e.g., `HashSet<OutPoint>`) referencing all outpoints
//!              belonging to that address
//!
//!    This partition is our **address index**, mapping each address to the set of outpoints owned by
//!    that address. When inserting or removing UTXOs, we keep this index in sync. Then, for lookups such
//!    as `get_utxos_for_address`, we can quickly retrieve the relevant outpoints without scanning all
//!    UTXOs.
//!
//! By maintaining these four partitions, we get efficient lookups for blocks, block heights, UTXOs by
//! outpoint, and addresses to outpoint sets.

#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199

mod error;
mod blocks;
mod utxo;


#[cfg(test)]
mod tests;

use std::path::PathBuf;
use fjall::{Config as FjallConfig, PartitionCreateOptions, Slice, TxKeyspace, TxPartition};
use tracing::info;

pub use crate::error::StryiStorageError;
pub use blocks::*;
pub use utxo::*;

use stryi_core::block::{Block, BlockHash};
use stryi_core::transactions::{OutPoint, UTXO};

/// `StryiStorage` manages four partitions within a single Fjall keyspace:
/// - `blocks_partition`: For storing blocks keyed by hash
/// - `heights_partition`: For storing mappings from height → hash
/// - `utxo_partition`: For storing actual UTXOs keyed by (txid+vout)
/// - `addresses_partition`: For mapping addresses → set of outpoints
///
/// Each partition is opened once at initialization, and we keep a reference in this struct.
pub struct StryiStorage {
    /// Partition storing blocks keyed by block hash
    pub blocks_partition: TxPartition,

    /// Partition storing block height → block hash
    pub heights_partition: TxPartition,

    /// Partition storing UTXOs
    pub utxo_partition: TxPartition,

    /// Partition storing address → set of OutPoints referencing that address
    pub addresses_partition: TxPartition,

    /// Keyspace for the entire database
    pub keyspace: TxKeyspace,
}

impl StryiStorage {
    /// Creates (or opens) the database at the given `path`, setting up four partitions:
    /// "blocks", "heights", "utxo", and "addresses".
    ///
    /// We open it in transactional mode so we can do atomic writes across multiple partitions.
    pub fn initialize_in_path(path: PathBuf) -> Result<Self, StryiStorageError> {
        info!("Trying to access Stryi storage at path {}", &path.display());

        // Create or open the KeySpace
        let cfg = FjallConfig::new(path).temporary(false);
        let keyspace = cfg.open_transactional()?;

        info!("Successfully initialized key space!");
        info!("Current database disk usage is : {} bytes", keyspace.disk_space());

        // Open or create the four partitions with default options
        let blocks_partition = keyspace.open_partition("blocks", PartitionCreateOptions::default())?;
        let heights_partition = keyspace.open_partition("heights", PartitionCreateOptions::default())?;
        let utxo_partition = keyspace.open_partition("utxo", PartitionCreateOptions::default())?;
        let addresses_partition = keyspace.open_partition("addresses", PartitionCreateOptions::default())?;

        // Construct and return our StryiStorage
        Ok(StryiStorage {
            blocks_partition,
            heights_partition,
            utxo_partition,
            addresses_partition,
            keyspace,
        })
    }
}
