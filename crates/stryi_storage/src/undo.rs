use fjall::Slice;
use futures::future::BoxFuture;
use stryi_core::block::BlockHash;
use stryi_core::BlockUndo;
use stryi_core::storage::UndoStorage;
use crate::{StryiStorage, StryiStorageError};


impl UndoStorage for StryiStorage {
    type StorageError = StryiStorageError;

    /// Stores undo data for a block in the undo partition.
    /// The key is the block hash, and the value is the serialized BlockUndo data.
    fn put_block_undo(&self, hash: BlockHash, undo: BlockUndo) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        
        Box::pin(async move {
            
            // Serialize BlockUndo using bincode
            let undo_bytes = bincode::serde::encode_to_vec(
                undo,
                bincode::config::standard()
            )?;

            // Store in undo partition using block hash as key
            self.undo_partition
                .insert(
                    Slice::from(&hash.data[..]),
                    Slice::from(undo_bytes),
                )
                .map_err(StryiStorageError::FjallError)?;


            Ok(())
        })
    }


    /// Retrieves undo data for a block from the undo partition.
    fn get_block_undo(
        &self,
        hash: BlockHash,
    ) -> BoxFuture<'_, Result<Option<BlockUndo>, Self::StorageError>> {
        Box::pin(async move {
            // Try to fetch the raw bytes for this block’s undo
            let maybe_raw = self
                .undo_partition
                .get(Slice::from(&hash.data[..]))
                .map_err(StryiStorageError::FjallError)?;

            // If there was no entry, we return Ok(None) instead of an error
            let raw = match maybe_raw {
                Some(bytes) => bytes,
                None => return Ok(None),
            };

            // Deserialize bytes into BlockUndo
            let (undo, _) =
                bincode::serde::decode_from_slice(&raw, bincode::config::standard())?;

            // Wrap in Some and return
            Ok(Some(undo))
        })
    }


    /// Removes undo data for a block from the undo partition.
    /// Does not return error if undo data doesn't exist.
    fn delete_block_undo(&self, hash: BlockHash) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        
        Box::pin(async move {
            self.undo_partition
                .remove(Slice::from(&hash.data[..]))
                .map_err(StryiStorageError::FjallError)?;

            Ok(())
        })
    }
}


#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use stryi_core::address::AccountAddress;
    use stryi_core::transactions::{OutPoint, TransactionHash, UTXO};
    use crate::blocks::tests::create_test_storage;
    use super::*;
    
    
    #[tokio::test]
    async fn test_block_undo_operations() -> Result<(), StryiStorageError> {
        let (storage, _temp_dir) = create_test_storage(false); // setup_state_storage is false

        
        // Create test block_hash and undo data
        let block_hash = BlockHash::new(&[1u8; 32]);
        let undo = BlockUndo {
            spent_utxos: HashSet::from([(
                    OutPoint {
                        txid: TransactionHash::new(&[1u8; 32]),
                        vout : 0,
                    },
                    
                    UTXO {
                        txid: TransactionHash::new(&[1u8; 32]),
                        vout: 0,
                        value: 500,
                        owner: AccountAddress::new(&[0u8; 20]),
                    }
                )]
            ),
            created_outpoints: HashSet::from(
                [
                    OutPoint {
                        txid: TransactionHash::new(&[1u8; 32]),
                        vout: 1,
                    },
                    OutPoint {
                        txid: TransactionHash::new(&[2u8; 32]),
                        vout: 2,
                    },
                    OutPoint {
                        txid: TransactionHash::new(&[3u8; 32]),
                        vout: 3,
                    },
                    OutPoint {
                        txid: TransactionHash::new(&[4u8; 32]),
                        vout: 4,
                    },

                ], 
            ),
        };

        // Test storing undo data
        storage.put_block_undo(block_hash, undo).await?;

        // Test retrieving undo data
        let retrieved = storage.get_block_undo(block_hash)
            .await?
            .expect("Undo *must* exist");
        assert_eq!(retrieved.spent_utxos.len(), 1);
        assert_eq!(retrieved.created_outpoints.len(), 4);

        // Test removing undo data
        storage.delete_block_undo(block_hash).await?;

        // Verify undo data is gone
        assert!(matches!(
            storage.get_block_undo(block_hash).await,
            Ok(None)
        ));

        Ok(())
    }
}