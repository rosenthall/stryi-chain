use bincode::config::standard;
use stryi_core::block::{Block, BlockHash};
use stryi_core::storage::BlockStorage;
use crate::error::StryiStorageError;
use crate::StryiStorage;
use fjall::{Slice, UserKey, UserValue};
use std::convert::TryFrom;
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

impl BlockStorage for StryiStorage {
    type StorageError = StryiStorageError;

    /// Retrieves a block by its hash.
    async fn get_block_by_hash(&self, hash: BlockHash) -> Result<Block, Self::StorageError> {
        let raw = self.blocks_partition
            .get(Slice::from(&hash.data[..]))
            .map_err(StryiStorageError::FjallError)?
            .ok_or_else(|| StryiStorageError::NotFound(format!("Block with hash {}", hash)))?;

        Self::deserialize_block(&raw)
    }

    /// Retrieves a block by its height.
    async fn get_block_by_height(&self, height: usize) -> Result<Block, Self::StorageError> {
        let height_key = Self::height_to_key(height)?;

        // First get the block hash from the height
        let block_hash_bytes = self.heights_partition
            .get(Slice::from(&height_key[..]))
            .map_err(StryiStorageError::FjallError)?
            .ok_or_else(|| StryiStorageError::NotFound(format!("Block at height {}", height)))?;

        // Convert bytes to BlockHash and get the block
        let block_hash = Self::bytes_to_block_hash(&block_hash_bytes)?;
        self.get_block_by_hash(block_hash).await
    }

    /// Retrieves the latest block in the chain.
    async fn get_latest_block(&self) -> Result<Block, Self::StorageError> {
        let result = self.heights_partition
            .last_key_value()
            .map_err(StryiStorageError::FjallError)?
            .ok_or_else(|| StryiStorageError::NotFound("Blockchain is empty".to_string()))?;

        let block_hash = Self::bytes_to_block_hash(&result.1)?;
        self.get_block_by_hash(block_hash).await
    }

    /// Inserts a new block or updates an existing one in the storage.
    async fn put_block(&mut self, block: &Block) -> Result<(), Self::StorageError> {

        // Create BlockUndo data first - if this fails, we won't proceed with block storage
        let undo_data = self.construct_block_undo(block).await?;
        let undo_bytes = bincode::serde::encode_to_vec(&undo_data, bincode::config::standard())?;


        let serialized_block = Self::serialize_block(block)?;
        let block_hash = block.block_hash();
        let height_key = Self::height_to_key(block.header.height as usize)?;
        

        // Get current state
        let current_state = self.get_current_storage_state()?;

        // Create new state
        let new_state = StorageStateInformation {
            latest_block: (block.header.height as usize, block_hash),
            last_update_time: block.header.timestamp as usize,
            blocks_count: current_state.blocks_count + 1,
            chain_difficulty: current_state.chain_difficulty + (1 << block.header.difficulty_bits),
        };

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

        // Update storage state value in stats_partition
        let state_key = UserKey::from([0u8; 32]);
        let state_value: UserValue = new_state.try_into()?;
        tx.insert(&self.stats_partition, state_key, state_value);


        // Commit the transaction
        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }

    /// Checks whether a block with the given hash exists.
    async fn block_exists(&self, hash: BlockHash) -> Result<bool, Self::StorageError> {
        Ok(self.blocks_partition
            .get(Slice::from(&hash.data[..]))
            .map_err(StryiStorageError::FjallError)?
            .is_some())
    }

    /// Retrieves the entire chain of blocks.
    async fn get_chain(&self) -> Result<Vec<Block>, Self::StorageError> {
        let state = self.get_current_storage_state()?;
        let total_blocks = state.blocks_count;

        let mut blocks = Vec::with_capacity(total_blocks);
        let mut current_height = 0usize;

        while current_height < total_blocks {
            match self.get_block_by_height(current_height).await {
                Ok(block) => {
                    blocks.push(block);
                    current_height += 1;
                },
                Err(StryiStorageError::NotFound(_)) => break,
                Err(e) => return Err(e),
            }
        }

        Ok(blocks)
    }

    /// Retrieves a range of blocks from `start_height` to `end_height` inclusive.
    /// Note: This performs sequential reads for the specified range
    async fn get_range(&self, start_height: usize, end_height: usize) -> Result<Vec<Block>, Self::StorageError> {
        if start_height > end_height {
            return Ok(Vec::new());
        }

        let mut blocks = Vec::with_capacity(end_height - start_height + 1);
        let mut current_height = start_height;

        while current_height <= end_height {
            match self.get_block_by_height(current_height).await {
                Ok(block) => blocks.push(block),
                Err(StryiStorageError::NotFound(_)) => break, // Stop at first gap
                Err(e) => return Err(e),
            }
            current_height += 1;
        }

        Ok(blocks)
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


        let mut storage = StryiStorage {
            keyspace,
            blocks_partition,
            heights_partition,
            utxo_partition,
            addresses_partition,
            stats_partition,
            undo_partition
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

        assert!(storage.block_exists(genesis.block_hash()).await?);
        let retrieved = storage.get_block_by_hash(genesis.block_hash()).await?;
        assert_eq!(retrieved.header.height, 0);
        assert_eq!(retrieved.header.is_genesis, true);

        // Test 2: Store more blocks
        for i in 1..=5 {
            storage.put_block(&create_test_block(i)).await?;
        }

        // Test 3: Get by height
        let block3 = storage.get_block_by_height(3).await?;
        assert_eq!(block3.header.height, 3);

        // Test 4: Latest block
        let latest = storage.get_latest_block().await?;
        assert_eq!(latest.header.height, 5);

        // Test 5: Get chain
        let chain = storage.get_chain().await?;
        assert_eq!(chain.len(), 6); // 0 through 5
        assert_eq!(chain[0].header.height, 0);
        assert_eq!(chain[5].header.height, 5);

        // Test 6: Get range
        let range = storage.get_range(2, 4).await?;
        assert_eq!(range.len(), 3);
        assert_eq!(range[0].header.height, 2);
        assert_eq!(range[2].header.height, 4);

        // Test 7: Empty range
        let empty_range = storage.get_range(4, 2).await?;
        assert!(empty_range.is_empty());


        // Check that storage state exists
        assert!(
            storage.get_current_storage_state().is_ok(),
        );


        Ok(())
    }

    #[tokio::test]
    async fn test_stryi_storage_gaps() -> Result<(), StryiStorageError> {
        // setup_state_storage argument is true since .get_chain() method utilizes storage_state api to get blocks_count
        let (mut storage, _temp_dir) = create_test_storage(true); 


        // Insert blocks 0,1,2,3,5 (gap at 4)
        for i in 0..=3 {
            storage.put_block(&create_test_block(i)).await?;
        }
        storage.put_block(&create_test_block(5)).await?;

        // Test: Range stops at gap
        let range = storage.get_range(2, 5).await?;
        assert_eq!(range.len(), 2); // Should contain only blocks 2 and 3
        assert_eq!(range[0].header.height, 2);
        assert_eq!(range[1].header.height, 3);

        // Test: Range after gap
        let empty_range = storage.get_range(4, 5).await?;
        assert!(empty_range.is_empty()); // Should be empty as it starts at a gap

        // Test: Get chain stops at first gap
        let chain = storage.get_chain().await?;
        assert_eq!(chain.len(), 4); // Should contain 0,1,2,3
        assert_eq!(chain[0].header.height, 0);
        assert_eq!(chain[3].header.height, 3);

        Ok(())
    }
}