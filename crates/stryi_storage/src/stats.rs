use crate::{StryiStorage, StryiStorageError};
use bincode::config::standard;
use fjall::{UserKey, UserValue};
use futures::future;
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use stryi_core::block::BlockHash;
use stryi_core::storage::StorageStats;

/// Struct with only purpose for storing current storage's stats
/// It meant to be serialized and deserialized after each new block (put_block method)
/// todo: Some pretty tables for StorageStateInformation via https://lib.rs/crates/prettytable-rs would be cool
#[derive(Clone, Serialize, Debug, Deserialize, PartialEq)]
pub struct StorageStateInformation {
    pub latest_block: (usize, BlockHash),
    pub last_update_time: usize,
    pub blocks_count: usize,
    pub chain_difficulty: usize,
}

// Define some helper impls

impl TryFrom<&UserValue> for StorageStateInformation {
    type Error = StryiStorageError;

    fn try_from(value: &UserValue) -> Result<Self, Self::Error> {
        let (storage_state, _): (StorageStateInformation, _) =
            bincode::serde::decode_from_slice(value, standard())?;

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
    pub fn get_current_storage_state(&self) -> Result<StorageStateInformation, StryiStorageError> {
        // the key for storage state is always just 256 zero bits
        let key = UserKey::from([0u8; 32]);

        self.stats_partition
            .get(key)?
            .ok_or_else(|| {
                StryiStorageError::NoStorageStatsFound("Storage state not initialized".to_string())
            })
            .and_then(|value| StorageStateInformation::try_from(&value))
    }

    pub(crate) fn update_storage_state(
        &mut self,
        state: StorageStateInformation,
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

    fn tip(&self) -> BoxFuture<'_, Result<(u64, BlockHash), Self::StorageError>> {
        Box::pin(future::ready(
            self.get_current_storage_state()
                .map(|s| (s.latest_block.0 as u64, s.latest_block.1)),
        ))
    }

    fn last_updated(&self) -> BoxFuture<'_, Result<u64, Self::StorageError>> {
        Box::pin(future::ready(
            self.get_current_storage_state()
                .map(|s| s.last_update_time as u64),
        ))
    }

    fn block_count(&self) -> BoxFuture<'_, Result<u64, Self::StorageError>> {
        Box::pin(future::ready(
            self.get_current_storage_state()
                .map(|s| s.blocks_count as u64),
        ))
    }

    fn chain_difficulty(&self) -> BoxFuture<'_, Result<u128, Self::StorageError>> {
        Box::pin(future::ready(
            self.get_current_storage_state()
                .map(|s| s.chain_difficulty as u128),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_storage_state_operations() -> Result<(), StryiStorageError> {
        // setup storage
        let (mut storage, _dir) = crate::blocks::tests::create_test_storage(false); // setup_state_storage is false

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
