use fjall::Slice;
use stryi_core::block::{Block, BlockHash};
use stryi_core::BlockUndo;
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::OutPoint;
use crate::{StryiStorage, StryiStorageError};

impl StryiStorage {

    /// Build `BlockUndo` for `block`, querying our own storage for every input.
    pub(crate) async fn construct_block_undo(
        &self,
        block: &Block,
    ) -> Result<BlockUndo, StryiStorageError> {

        // Create lookup closure that uses our storage's get_utxo 
        // lookup : &OutPoint -> impl Future<Output = Option<UTXO>>
        let lookup = |op: &OutPoint| {
            let op = op.clone();          // move into async block
            async move {
                match self.get_utxo(op).await {
                    Ok(utxo) => Some(utxo), 
                    Err(StryiStorageError::NotFound(_)) => None,
                    Err(e) => {
                        tracing::error!("UTXO lookup error: {e:?}");
                        None  
                    }
                }?
            }
        };

        block
            .create_undo(lookup)
            .await
            .map_err(|e| StryiStorageError::UndoCreationError {
                msg: format!("Failed to create undo data: {e}"),
            })
    }



    /// Stores undo data for a block in the undo partition.
    /// The key is the block hash, and the value is the serialized BlockUndo data.
    ///
    /// # Arguments
    /// * `block_hash` - Hash of the block for which to store undo data
    /// * `undo` - The BlockUndo data to store
    pub(crate) async fn store_block_undo(
        &mut self,
        block_hash: &BlockHash,
        undo: &BlockUndo,
    ) -> Result<(), StryiStorageError> {
        // Serialize BlockUndo using bincode
        let undo_bytes = bincode::serde::encode_to_vec(
            undo,
            bincode::config::standard()
        )?;

        // Store in undo partition using block hash as key
        self.undo_partition
            .insert(
                Slice::from(&block_hash.data[..]),
                Slice::from(undo_bytes),
            )
            .map_err(StryiStorageError::FjallError)?;

        Ok(())
    }

    /// Retrieves undo data for a block from the undo partition.
    /// Returns NotFound error if no undo data exists for the given block hash.
    ///
    /// # Arguments
    /// * `block_hash` - Hash of the block whose undo data to retrieve
    pub(crate) async fn get_block_undo(
        &self,
        block_hash: &BlockHash,
    ) -> Result<BlockUndo, StryiStorageError> {
        // Get raw bytes from undo partition
        let raw = self.undo_partition
            .get(Slice::from(&block_hash.data[..]))
            .map_err(StryiStorageError::FjallError)?
            .ok_or_else(|| StryiStorageError::NotFound(
                format!("Undo data not found for block {}", block_hash)
            ))?;

        // Deserialize bytes into BlockUndo
        let (undo, _) = bincode::serde::decode_from_slice(
            &raw,
            bincode::config::standard()
        )?;

        Ok(undo)
    }

    /// Removes undo data for a block from the undo partition.
    /// Does not return error if undo data doesn't exist.
    ///
    /// # Arguments
    /// * `block_hash` - Hash of the block whose undo data to remove
    pub(crate) async fn remove_block_undo(
        &mut self,
        block_hash: &BlockHash,
    ) -> Result<(), StryiStorageError> {
        self.undo_partition
            .remove(Slice::from(&block_hash.data[..]))
            .map_err(StryiStorageError::FjallError)?;

        Ok(())
    }
}



#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use crate::blocks::tests::create_test_storage;
    use super::*;
    
    
    #[tokio::test]
    async fn test_block_undo_operations() -> Result<(), StryiStorageError> {
        let (mut storage, _temp_dir) = create_test_storage(false); // setup_state_storage is false

        // Create test block hash and undo data
        let block_hash = BlockHash::new(&[1u8; 32]);
        let undo = BlockUndo {
            spent_utxos: HashSet::new(),
            created_outpoints: HashSet::new(),
        };

        // Test storing undo data
        storage.store_block_undo(&block_hash, &undo).await?;

        // Test retrieving undo data
        let retrieved = storage.get_block_undo(&block_hash).await?;
        assert_eq!(retrieved.spent_utxos.len(), 0);
        assert_eq!(retrieved.created_outpoints.len(), 0);

        // Test removing undo data
        storage.remove_block_undo(&block_hash).await?;

        // Verify undo data is gone
        assert!(matches!(
            storage.get_block_undo(&block_hash).await,
            Err(StryiStorageError::NotFound(_))
        ));

        Ok(())
    }
}