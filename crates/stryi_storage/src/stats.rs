use bincode::config::standard;
use fjall::{UserKey, UserValue};
use serde::{Deserialize, Serialize};
use stryi_core::block::BlockHash;
use stryi_core::storage::StorageStats;
use crate::{StryiStorage, StryiStorageError};

/// Private struct, the only purpose it has - store current storage's stats
/// It meant to be serialized and deserialized after each new block (put_block method)
#[derive(Clone, Serialize, Debug, Deserialize, PartialEq)]
pub struct StorageStateInformation {
    pub(crate) latest_block : (usize, BlockHash),
    pub(crate) last_update_time : usize,
    pub(crate) blocks_count : usize,
    pub(crate) chain_difficulty : usize,
}


// Define some helper impls


impl TryFrom<&UserValue> for StorageStateInformation {
    type Error = StryiStorageError;

    fn try_from(value: &UserValue) -> Result<Self, Self::Error> {
        let (storage_state, _) : (StorageStateInformation, _) = bincode::serde::decode_from_slice(value, standard())?;

        Ok(storage_state)

    }
}


impl TryInto<UserValue> for StorageStateInformation {
    type Error = StryiStorageError;

    fn try_into(self) -> Result<UserValue, Self::Error> {
        
        let buf = bincode::serde::encode_to_vec(self, standard())?;
        
        Ok(UserValue::new(&buf))
    }
}



impl StryiStorage {
    pub(crate) fn get_current_storage_state(&self) -> Result<StorageStateInformation, StryiStorageError> {

        // the key for storage state is always just 256 zero bits
        let key = UserKey::from([0u8; 32]);

        self
            .stats_partition
            .get(key)?
            .ok_or_else(|| StryiStorageError::NoStorageStatsFound("Storage state not initialized".to_string()))
            .and_then(|value| StorageStateInformation::try_from(&value))
    }

    pub(crate) fn update_storage_state(
        &mut self,
        state: StorageStateInformation
    ) -> Result<(), StryiStorageError> {
        let key = UserKey::from([0u8; 32]);

        // Convert state to UserValue
        let value: UserValue = state.try_into()?;

        // Insert into partition
        self.stats_partition
            .insert(key, value)
            .map_err(StryiStorageError::FjallError)?;

        Ok(())
    }

}



impl StorageStats for StryiStorage {
    type StorageError = StryiStorageError;

    async fn get_latest_block(&self) -> Result<(usize, BlockHash), Self::StorageError> {
        let state = self.get_current_storage_state()?;
        Ok(state.latest_block)
    }

    async fn get_last_update_time(&self) -> Result<usize, Self::StorageError> {
        let state = self.get_current_storage_state()?;
        Ok(state.last_update_time)
        
    }

    async fn get_blocks_count(&self) -> Result<usize, Self::StorageError> {
        let state = self.get_current_storage_state()?;
        Ok(state.blocks_count)
    }

    async fn get_chain_difficulty(&self) -> Result<usize, Self::StorageError>  { 
        let state = self.get_current_storage_state()?;
        Ok(state.chain_difficulty)
    }
}




#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use fjall::{Config, PartitionCreateOptions};
    
    #[test]
    fn test_storage_state_operations() -> Result<(), StryiStorageError> {
        // Create temporary directory for testing
        let temp_dir = TempDir::new().expect("Failed to create temp directory");

        // Initialize storage with required partitions
        let keyspace = Config::new(temp_dir.path())
            .temporary(true)
            .open_transactional()
            .expect("Failed to open keyspace");

        let stats_partition = keyspace
            .open_partition("stats", PartitionCreateOptions::default())
            .expect("Failed to create stats partition");

        // Other required partitions for StryiStorage
        let blocks_partition = keyspace
            .open_partition("blocks", PartitionCreateOptions::default())
            .expect("Failed to create blocks partition");

        let heights_partition = keyspace
            .open_partition("heights", PartitionCreateOptions::default())
            .expect("Failed to create heights partition");

        let utxo_partition = keyspace
            .open_partition("utxo", PartitionCreateOptions::default())
            .expect("Failed to create utxo partition");

        let undo_partition = keyspace
            .open_partition("undo", PartitionCreateOptions::default())
            .expect("Failed to create undo partition");
        
        let addresses_partition = keyspace
            .open_partition("addresses", PartitionCreateOptions::default())
            .expect("Failed to create addresses partition");

        let mut storage = StryiStorage {
            keyspace,
            blocks_partition,
            heights_partition,
            utxo_partition,
            addresses_partition,
            stats_partition,
            undo_partition
        };

        // Initially, storage state should not exist
        assert!(matches!(
            storage.get_current_storage_state(),
            Err(StryiStorageError::NoStorageStatsFound(_))
        ));

        // Create and store test state
        let test_state = StorageStateInformation {
            latest_block: (1, BlockHash::empty()),
            last_update_time: 12345,
            blocks_count: 1,
            chain_difficulty: 100,
        };

        // Update storage state
        storage.update_storage_state(test_state.clone())?;

        // Retrieve and verify state
        let retrieved_state = storage.get_current_storage_state()?;
        assert_eq!(retrieved_state, test_state);

        Ok(())
    }
}