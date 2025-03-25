use serde::{Deserialize, Serialize};
use crate::{StryiStorage, StryiStorageError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockIndexData {
    pub parent_hash: stryi_core::block::BlockHash,
    pub height: u64,
    pub chain_work: u128,
}

impl StryiStorage {
    /// Inserts or updates the block index data for a given block hash.
    pub fn put_block_index(
        &mut self,
        block_hash: &stryi_core::block::BlockHash,
        index_data: &BlockIndexData
    ) -> Result<(), StryiStorageError> {
        let mut tx = self.keyspace.write_tx();

        // Serialize
        let encoded = bincode::serde::encode_to_vec(index_data, bincode::config::standard())?;
        tx.insert(
            &self.block_index_partition,
            fjall::Slice::from(&block_hash.data),
            fjall::Slice::from(encoded),
        );

        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }

    /// Retrieves the block index data for a given block hash, if it exists.
    pub fn get_block_index(
        &self,
        block_hash: &stryi_core::block::BlockHash
    ) -> Result<BlockIndexData, StryiStorageError> {
        let raw_opt = self.block_index_partition
            .get(fjall::Slice::from(&block_hash.data))
            .map_err(StryiStorageError::FjallError)?;

        let raw = match raw_opt {
            Some(val) => val,
            None => return Err(StryiStorageError::NotFound(
                format!("No block index entry for hash {}", block_hash)
            )),
        };

        let (decoded, _) = bincode::serde::decode_from_slice::<BlockIndexData, _>(
            &raw,
            bincode::config::standard()
        )?;
        Ok(decoded)
    }

    /// Checks if an index record exists for the given hash.
    pub fn has_block_index(
        &self,
        block_hash: &stryi_core::block::BlockHash
    ) -> Result<bool, StryiStorageError> {
        let opt = self.block_index_partition
            .get(fjall::Slice::from(&block_hash.data))
            .map_err(StryiStorageError::FjallError)?;

        Ok(opt.is_some())
    }

    /// Removes an index entry for a block hash (if doing detach from the main chain).
    pub fn remove_block_index(
        &mut self,
        block_hash: &stryi_core::block::BlockHash
    ) -> Result<(), StryiStorageError> {
        let mut tx = self.keyspace.write_tx();
        tx.remove(&self.block_index_partition, fjall::Slice::from(&block_hash.data));
        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }
}
