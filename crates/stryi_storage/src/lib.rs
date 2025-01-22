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
//! 2. Depth
//!
//! Key   : unsigned int (depth)
//! Value : [`stryi_core::block::BlockHash`]
//!
//!     What:
//!     Maps each block’s depth to its BlockHash.
//!     Why:
//!     Enables quick block lookups by depth for navigation and validation.
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

use std::path::PathBuf;
use bincode::config::standard;
use fjall::{Config, PartitionCreateOptions, TxPartition};
use tracing::info;

pub use crate::error::StryiStorageError;
use stryi_core::block::{Block, BlockHash};
use stryi_core::transactions::{OutPoint, UTXO};

/// The database uses a key-value storage model with 3 separate partitions:
///   1. Blocks      (BlockHash -> bincode(Block))
///   2. Depth       (depth u64 -> BlockHash)
///   3. UTXO        (String "tx_hash:index" -> bincode(UTXO))
pub struct StryiStorage {
    /// Partition storing blocks keyed by stringified BlockHash
    blocks_partition: TxPartition,

    /// Partition mapping a depth (u64) to a BlockHash
    depths_partition: TxPartition,

    /// Partition storing unspent transaction outputs
    utxo_partition: TxPartition,

}

impl StryiStorage {
    /// Opens (or creates) the database at the given `path`, and sets up three partitions:
    ///   "blocks", "depth", "utxo"
    pub fn try_from_path(path: PathBuf) -> Result<Self, StryiStorageError> {
        info!("Trying to access Stryi storage at path {}", &path.display());

        // Create or open the KeySpace
        let cfg = Config::new(path)
            .temporary(false);

        // Use transactional mode
        let keyspace = cfg.open_transactional()?;

        info!("Successfully initialized key space!");
        info!("Current database disk usage is : {} bytes", keyspace.disk_space());

        // Open or create the three partitions with default options
        let blocks_partition = keyspace.open_partition("blocks", PartitionCreateOptions::default())?;
        let depths_partition = keyspace.open_partition("depth", PartitionCreateOptions::default())?;
        let utxo_partition = keyspace.open_partition("utxo", PartitionCreateOptions::default())?;

        // Now we can build our storage struct
        Ok(StryiStorage {
            blocks_partition,
            depths_partition,
            utxo_partition,
        })
    }

    // -----------------------------------------------------
    // 1) BLOCKS PARTITION
    // -----------------------------------------------------

    /// Stores a block under its BlockHash as the key.
    pub fn put_block(&self, block: &Block) -> Result<(), StryiStorageError> {
        let key = block.block_hash().to_string();
        // for example: "Bxabcdef1234..."
        let value = bincode::serde::encode_to_vec(block, standard())
            .map_err(|err| StryiStorageError::SerializationError(err))?;

        self.blocks_partition
            .insert(key.as_bytes(), &value)
            .map_err(|err| StryiStorageError::FjallError(err))?;
        

        Ok(())
    }
    
    /// Fetches a block by its BlockHash (returns error if not found).
    pub fn get_block_by_hash(&self, hash: &BlockHash) -> Result<Block, StryiStorageError> {
        let key = hash.to_string();
        let raw = self
            .blocks_partition
            .get(key.as_bytes())
            .map_err(|err| StryiStorageError::FjallError(err))?
            .ok_or_else(|| StryiStorageError::NotFound(format!("BlockHash {}", key)))?;
    
        let block: Block = bincode::serde::decode_from_slice(&raw, standard())
            .map_err(|err| StryiStorageError::DeserializationError(err))?
            .0;
        
        Ok(block)
    }
    
    // -----------------------------------------------------
    // 2) DEPTH PARTITION
    // -----------------------------------------------------
    
    
    
    /// Maps a depth (u64) to a given BlockHash
    pub fn put_block_depth(&self, depth: u64, block_hash: &BlockHash) -> Result<(), StryiStorageError> {
        let depth_key = depth.to_be_bytes(); // convert u64 -> [u8;8]
        let value = block_hash.to_string().into_bytes();
    
        self.depths_partition
            .insert(&depth_key, &value)
            .map_err(|err| StryiStorageError::FjallError(err))?;
    
        Ok(())
    }
    
    /// Fetches a block hash by depth (returns error if not found).
    pub fn get_block_hash_by_depth(&self, depth: u64) -> Result<BlockHash, StryiStorageError> {
        let depth_key = depth.to_be_bytes();
        let raw = self
            .depths_partition
            .get(&depth_key)
            .map_err(|err| StryiStorageError::FjallError(err))?
            .ok_or_else(|| StryiStorageError::NotFound(format!("Depth {}", depth)))?;
    

        let hash_string = String::from_utf8(raw.to_vec())?;


        // Now parse the block hash from string
        let block_hash = BlockHash::from_hash_string(&hash_string)
            .map_err(|e| StryiStorageError::IncorrectHashValue(e.to_string()))?;
        
        Ok(block_hash)
    }
    
    // // -----------------------------------------------------
    // // 3) UTXO PARTITION
    // // -----------------------------------------------------
    //
    /// Create a key like "Txabcdef...:2" for outpoint
    fn make_utxo_key(tx_hash_str: &str, vout: u32) -> String {
        // e.g. "Txabcdef1234:0"
        format!("{}:{}", tx_hash_str, vout)
    }
    
    
    // /// Puts a UTXO in the store keyed by (tx_hash:index).
    pub fn put_utxo(&self, outpoint: &OutPoint, utxo: &UTXO) -> Result<(), StryiStorageError> {
        let key = Self::make_utxo_key(&outpoint.txid.to_string(), outpoint.vout);
        let value = bincode::serde::encode_to_vec(utxo, standard())
            .map_err(|err| StryiStorageError::SerializationError(err))?;
    
        self.utxo_partition
            .insert(key.as_bytes(), &value)
            .map_err(|err| StryiStorageError::FjallError(err))?;
        
        Ok(())
    }
    
    /// Fetches a UTXO by outpoint, returns error if not found.
    pub fn get_utxo(&self, outpoint: &OutPoint) -> Result<UTXO, StryiStorageError> {
        let key = Self::make_utxo_key(&outpoint.txid.to_string(), outpoint.vout);
        let raw = self
            .utxo_partition
            .get(key.as_bytes())
            .map_err(|err| StryiStorageError::FjallError(err))?
            .ok_or_else(|| StryiStorageError::NotFound(format!("UTXO {key}")))?;
    
        let (utxo, _) = bincode::serde::decode_from_slice(&raw, standard())
            .map_err(|err| StryiStorageError::DeserializationError(err))?;
        Ok(utxo)
    }
    
    /// Removes a UTXO from the store (if present).
    pub fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), StryiStorageError> {
        let key = Self::make_utxo_key(&outpoint.txid.to_string(), outpoint.vout);
        self.utxo_partition
            .remove(key.as_bytes())
            .map_err(|err| StryiStorageError::FjallError(err))?;
    
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;
    use stryi_core::transactions::{TransactionHash};
    use stryi_core::address::AccountAddress;

    #[test]
    fn test_stryi_storage_basics() {
        let mut path = temp_dir();
        path.push("stryichain_fjall_db_test");

        // create a new StryiStorage
        let storage = StryiStorage::try_from_path(path).expect("Failed to open DB");

        // 1) test block partition
        let dummy_block = Block {
            header: stryi_core::block::BlockHeader {
                version: 1,
                merkle_root_hash: [0u8; 32],
                previous_block_hash: BlockHash::empty(),
                height: 42,
                difficulty_bits: 7,
                timestamp: 1234567,
                nonce: 999,
            },
            data: stryi_core::block::BlockData {
                transactions: vec![],
            },
        };

        // store
        storage.put_block(&dummy_block).unwrap();
        // fetch
        let fetched_block = storage.get_block_by_hash(&dummy_block.block_hash()).unwrap();
        assert_eq!(fetched_block.header.height, 42);

        // 2) test depth partition
        storage.put_block_depth(42, &dummy_block.block_hash()).unwrap();
        let found_hash = storage.get_block_hash_by_depth(42).unwrap();
        assert_eq!(found_hash, dummy_block.block_hash());

        // 3) test UTXO partition
        let dummy_outpoint = stryi_core::transactions::OutPoint {
            txid: TransactionHash::new(&[9u8;32]),
            vout: 0,
        };
        let dummy_utxo = stryi_core::transactions::UTXO {
            txid: dummy_outpoint.txid,
            vout: dummy_outpoint.vout,
            value: 777,
            owner: AccountAddress::new(&[1u8;20]),
        };

        storage.put_utxo(&dummy_outpoint, &dummy_utxo).unwrap();
        let fetched_utxo = storage.get_utxo(&dummy_outpoint).unwrap();
        assert_eq!(fetched_utxo.value, 777);

        // remove
        storage.remove_utxo(&dummy_outpoint).unwrap();
        let missing = storage.get_utxo(&dummy_outpoint);
        assert!(missing.is_err(), "UTXO should be removed");
    }
}
