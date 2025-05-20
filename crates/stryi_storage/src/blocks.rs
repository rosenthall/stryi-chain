use std::collections::HashMap;
use bincode::config::standard;
use stryi_core::block::{Block, BlockHash};
use stryi_core::storage::BlockStorage;
use crate::error::StryiStorageError;
use crate::StryiStorage;
use fjall::{Slice, UserKey, UserValue};
use std::convert::TryFrom;
use std::range::RangeInclusive;
use futures::future::BoxFuture;
use crate::index::BlockIndexData;
use crate::stats::StorageStateInformation;

impl StryiStorage {
    /// Converts a block height to a database key
    fn height_to_key(height: usize) -> Result<[u8; 8], StryiStorageError> {
        u64::try_from(height)
            .map(|h| h.to_be_bytes())
            .map_err(|_| StryiStorageError::InvalidHeight(height))
    }

    /// Converts bytes from database into BlockHash
    fn bytes_to_block_hash(bytes: &[u8]) -> Result<BlockHash, StryiStorageError> {
        BlockHash::try_from(bytes.to_vec())
            .map_err(|e| StryiStorageError::IncorrectHashValue(
                format!("Invalid block hash data in database: {}", e)
            ))
    }

    /// Serialize block for storage
    fn serialize_block(block: &Block) -> Result<Vec<u8>, StryiStorageError> {
        bincode::serde::encode_to_vec(block, standard())
            .map_err(StryiStorageError::SerializationError)
    }

    /// Deserialize block from storage
    fn deserialize_block(data: &[u8]) -> Result<Block, StryiStorageError> {
        let (block, _) = bincode::serde::decode_from_slice(data, standard())
            .map_err(StryiStorageError::DeserializationError)?;
        Ok(block)
    }
}




 impl BlockStorage for StryiStorage  {
     type StorageError = StryiStorageError;


     fn put_block(&mut self, block: &Block) -> BoxFuture<Result<(), Self::StorageError>> {
         let block = block.clone();

         Box::pin(
             async move {
                 // Inserts a new block or updates an existing one in the storage.
                 let undo_data = self.construct_block_undo(&block).await?;
                 let undo_bytes = bincode::serde::encode_to_vec(&undo_data, standard())?;

                 // Serialize the block itself
                 let serialized_block = Self::serialize_block(&block)?;
                 let height_key = Self::height_to_key(block.header.height as usize)?;

                 // Compute the block hash from the block
                 let block_hash = block.block_hash();

                 // Read current state so we can update chain stats
                 let current_state = self.get_current_storage_state()?;

                 // Compute new chain difficulty, etc.
                 let new_chain_diff = current_state.chain_difficulty + (1 << block.header.difficulty_bits);
                 let new_state = StorageStateInformation {
                     latest_block: (block.header.height as usize, block_hash),
                     last_update_time: block.header.timestamp as usize,
                     blocks_count: current_state.blocks_count + 1,
                     chain_difficulty: new_chain_diff,
                 };

                 // Prepare BlockIndexData
                 let index_data = BlockIndexData {
                     parent_hash: block.header.previous_block_hash,
                     height: block.header.height,
                     chain_work: new_chain_diff as u128,
                 };

                 let index_bytes = bincode::serde::encode_to_vec(&index_data, standard())?;
                 // Create a write transaction. We will update blocks, heights, undo and state partitions by just one transaction
                 let mut tx = self.keyspace.write_tx();

                 // Store block data in blocks partition
                 tx.insert(
                     &self.blocks_partition,
                     Slice::from(&block_hash.data[..]),
                     Slice::from(serialized_block),
                 );

                 // Store height mapping in heights partition
                 tx.insert(
                     &self.heights_partition,
                     Slice::from(&height_key[..]),
                     Slice::from(&block_hash.data[..]),
                 );


                 // Store undo data in undo partition
                 tx.insert(
                     &self.undo_partition,
                     Slice::from(&block_hash.data[..]),
                     Slice::from(undo_bytes),
                 );

                 // Store block index entry in block_index partition
                 tx.insert(
                     &self.block_index_partition,
                     Slice::from(&block_hash.data[..]),
                     Slice::from(index_bytes),
                 );

                 // Update storage state value in stats_partition
                 let state_key = UserKey::from([0u8; 32]);
                 let state_value: UserValue = new_state.try_into()?;
                 tx.insert(&self.stats_partition, state_key, state_value);


                 // Commit the transaction
                 tx.commit().map_err(StryiStorageError::FjallError)?;

                 // Exit
                 Ok(())
             }
         )
     }
     
     fn batch_get_blocks_by_hashes(&self, hashes: Vec<BlockHash>) -> BoxFuture<Result<HashMap<BlockHash, Block>, Self::StorageError>> {
         // in fact this method is not performing *real* batch-read but just reading blocks ony-by-one, so batching is only api-level thing.
         Box::pin(async move {
             let mut result_map = HashMap::new();
             for hash in hashes {
                 let raw = self.blocks_partition
                     .get(Slice::from(&hash.data[..]))
                     .map_err(StryiStorageError::FjallError)?;
                 if let Some(bytes) = raw {
                     let block = StryiStorage::deserialize_block(&bytes)?;
                     result_map.insert(hash, block);
                 }
             }
             Ok(result_map)
         })
     }



     fn batch_get_blocks_by_heights<I>(&self, heights: I) -> BoxFuture<Result<HashMap<u64, Block>, Self::StorageError>>
     where
         I: IntoIterator<Item = u64> + Send,
         I::IntoIter: Send,
     {

         let heights_partition = self.heights_partition.clone();
         let blocks_partition = self.blocks_partition.clone();
         let heights_vec: Vec<u64> = heights.into_iter().collect();


         Box::pin(async move {
             let mut result_map = HashMap::new();
             for height in heights_vec {
                 let key = StryiStorage::height_to_key(height as usize)?;
                 if let Some(hash_bytes) = heights_partition.get(Slice::from(&key[..]))
                     .map_err(StryiStorageError::FjallError)?
                 {
                     let hash = StryiStorage::bytes_to_block_hash(&hash_bytes)?;
                     if let Some(bytes) = blocks_partition.get(Slice::from(&hash.data[..]))
                         .map_err(StryiStorageError::FjallError)?
                     {
                         let block = StryiStorage::deserialize_block(&bytes)?;
                         result_map.insert(height, block);
                     }
                 }
             }
             Ok(result_map)
         })
     }

     fn blocks_range(&self, range: RangeInclusive<usize>) -> BoxFuture<Result<HashMap<u64, Block>, Self::StorageError>> {

         let blocks_partition = self.blocks_partition.clone();
         let heights_partition = self.heights_partition.clone();

         Box::pin(async move {
             // Early return if range is invalid
             StryiStorage::validate_range(range)?;

             // Setup result map
             let len = range.into_iter().count();
             let mut result_map = HashMap::with_capacity(len);

             // Convert i32 to u64 in range for compatibility with return type
             for height in range.iter().map(|n| n as u64) {

                 // Get BlockHash by height
                 let key = StryiStorage::height_to_key(height as usize)?;
                 match heights_partition.get(Slice::from(&key[..])).map_err(StryiStorageError::FjallError)? {

                     // Return error if unable to find a BlockHash by height.
                     None => return Err(StryiStorageError::NotFound(format!("BlockHash of block with height: {height}"))),


                     Some(hash_bytes) => {

                         //  Try to get entire Block by hash we got.
                         let hash = StryiStorage::bytes_to_block_hash(&hash_bytes)?;
                         match blocks_partition.get(Slice::from(&hash.data[..])).map_err(StryiStorageError::FjallError)? {

                             // Return error if unable to find a Block by hash.
                             None => return Err(StryiStorageError::NotFound(format!("block with hash: {hash}"))),

                             Some(block_bytes) => {
                                 let block = StryiStorage::deserialize_block(&block_bytes)?;
                                 result_map.insert(height, block);
                             },
                         }
                     }
                 }
             }
             // Return map
             Ok(result_map)
         })

     }

     fn block_exists(&self, hash: BlockHash) -> BoxFuture<Result<bool, Self::StorageError>> {
         Box::pin(async move {
             let present = self.blocks_partition
                 .get(Slice::from(&hash.data[..]))
                 .map_err(StryiStorageError::FjallError)?
                 .is_some();
             Ok(present)
         })
     }
 }

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::TempDir;
    use fjall::{Config, PartitionCreateOptions};
    use stryi_core::block::{Block, BlockHeader};

    /// Helper function to create a test block with given height
    fn create_test_block(height: usize) -> Block {
        Block {
            header: BlockHeader {
                version: 1,
                merkle_root_hash: [0u8; 32],
                previous_block_hash: BlockHash::empty(),
                height: height as u64,
                difficulty_bits: 1,
                timestamp: 1234567,
                nonce: height as u32,
                is_genesis: height == 0,
            },
            data: stryi_core::block::BlockData {
                transactions: vec![],
            },
        }
    }

    /// Helper function to create StryiStorage with temp directory
    pub fn create_test_storage(setup_state_storage : bool) -> (StryiStorage, TempDir) {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let keyspace = Config::new(temp_dir.path())
            .temporary(true)
            .open_transactional()
            .expect("Failed to open keyspace");

        let blocks_partition = keyspace
            .open_partition("blocks", PartitionCreateOptions::default())
            .expect("Failed to create blocks partition");

        let heights_partition = keyspace
            .open_partition("heights", PartitionCreateOptions::default())
            .expect("Failed to create heights partition");

        let utxo_partition = keyspace
            .open_partition("utxo", PartitionCreateOptions::default())
            .expect("Failed to create utxo partition");

        let addresses_partition = keyspace
            .open_partition("addresses", PartitionCreateOptions::default())
            .expect("Failed to create addresses partition");

        let stats_partition = keyspace
            .open_partition("stats", PartitionCreateOptions::default())
            .expect("Failed to create stats partition");
        
        let undo_partition = keyspace
            .open_partition("undo", PartitionCreateOptions::default())
            .expect("Failed to create undo partition");


        let block_index_partition = keyspace
            .open_partition("block_indexes", PartitionCreateOptions::default())
            .expect("Failed to create undo partition");


        let mut storage = StryiStorage {
            keyspace,
            blocks_partition,
            heights_partition,
            utxo_partition,
            addresses_partition,
            stats_partition,
            undo_partition,
            block_index_partition
        };


        
        // Some tests require correct storage state, while some of them creating own, so I kept this optional
        if setup_state_storage {
            // Create an empty initial state
            let initial_state = StorageStateInformation {
                latest_block: (0, BlockHash::empty()),
                last_update_time: 0,
                blocks_count: 0,
                chain_difficulty: 0,
            };
            
            // Store initial state
            storage.update_storage_state(initial_state).unwrap();
        }


        (storage, temp_dir)
    }

    #[tokio::test]
    async fn test_stryi_block_storage_basics() -> Result<(), StryiStorageError> {
        let (mut storage, _temp_dir) = create_test_storage(true);

        // Test 1: Store and retrieve genesis block
        let genesis = create_test_block(0);
        storage.put_block(&genesis).await?;

        let retrieved = storage.get_block_by_hash(genesis.block_hash()).await?.expect("Must return `Some(block)`");
        assert_eq!(retrieved.header.height, 0);
        assert!(retrieved.header.is_genesis);

        // Test 2: Store more blocks
        for i in 1..=5 {
            storage.put_block(&create_test_block(i)).await?;
        }

        // Test 3: Get by height
        let block3 = storage.get_block_by_height(3).await?.expect("Must return `Some(block)`");
        assert_eq!(block3.header.height, 3);

        // Test 4: Latest block TODO: refactor get_latest_block as well
        // let latest = storage.get_latest_block().await?;
        // assert_eq!(latest.header.height, 5);

        // Test 5: Get valid range 
        let range = storage.blocks_range(RangeInclusive::from(2..=4)).await?;
        assert_eq!(range.len(), 3);
        
        assert_eq!(range[&2].header.height, 2);
        assert_eq!(range[&3].header.height, 3);
        assert_eq!(range[&4].header.height, 4);
        
        
        // Check that storage state exists
        assert!(
            storage.get_current_storage_state().is_ok(),
        );


        Ok(())
    }

    #[tokio::test]
    async fn test_stryi_storage_gaps() -> Result<(), StryiStorageError> {
        let (mut storage, _temp_dir) = create_test_storage(true); 


        // Insert blocks 0,1,2,3,5 (gap at 4)
        for i in 0..=3 {
            storage.put_block(&create_test_block(i)).await?;
        }
        storage.put_block(&create_test_block(5)).await?;

        // Test: Range that includes gap
        let empty_range = storage.blocks_range(RangeInclusive::from(3..=5)).await;
        assert!(empty_range.is_err()); // should return error because of a gap

        Ok(())
    }
}