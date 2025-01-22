use std::error::Error;
use std::fmt::Debug;
use crate::address::AccountAddress;
use crate::block::{Block, BlockHash};
use crate::transactions::{OutPoint, UTXO};

/// Trait representing storage backend for UTXOs.
pub trait UtxoStorage: Send + Sync {
     type StorageError: Debug + Error + Clone + Send;
    
    /// Gets all the UTXOs for the provided AccountAddress.
    /// Helpful for calculating account balance.
    async fn get_utxos_for_address(&self, address: &AccountAddress)
                                   -> Result<Vec<(OutPoint, UTXO)>, Self::StorageError>;

    /// Returns the UTXO for the specified outpoint if unspent,
    /// or returns an error if not found or already spent.
    async fn get_utxo(&self, outpoint: &OutPoint)
                      -> Result<UTXO, Self::StorageError>;

    /// Inserts or updates a UTXO in storage.
    async fn put_utxo(&mut self, outpoint: &OutPoint, utxo: UTXO)
                      -> Result<(), Self::StorageError>;

    /// Removes a UTXO from storage (marks it as spent).
    async fn remove_utxo(&mut self, outpoint: &OutPoint)
                         -> Result<(), Self::StorageError>;

    /// Batch inserts or updates multiple UTXOs in storage.
    async fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>
    ) -> Result<(), Self::StorageError>;

    /// Batch removes multiple UTXOs from storage (marks them as spent).
    async fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>
    ) -> Result<(), Self::StorageError>;
}




/// Trait representing storage backend for Blocks.
pub trait BlockStorage: Send + Sync {
    type StorageError: Debug + Error + Clone + Send;

    /// Retrieves a block by its hash.
    async fn get_block_by_hash(&self, hash: BlockHash) -> Result<Block, Self::StorageError>;

    /// Retrieves a block by its height (index).
    async fn get_block_by_height(&self, height: usize) -> Result<Block, Self::StorageError>;

    /// Retrieves the latest (most recent) block in the chain.
    async fn get_latest_block(&self) -> Result<Block, Self::StorageError>;

    /// Inserts a new block or updates an existing one in the storage 
    async fn put_block(&mut self, block: &Block) -> Result<(), Self::StorageError>;

    /// Checks whether a block with the given hash exists.
    async fn block_exists(&self, hash: &str) -> Result<bool, Self::StorageError>;
    
    /// Retrieves the entire chain of blocks.
    async fn get_chain(&self) -> Result<Vec<Block>, Self::StorageError>;

    /// Retrieves a range of blocks from `start_height` to `end_height` inclusive.
    async fn get_range(&self, start_height: usize, end_height: usize)
                       -> Result<Vec<Block>, Self::StorageError>;
}




/// Simple implementation of UtxoStorage trait. Uses HashMap + RwLock inside
/// Created to simplify some steps in development. 
/// The implementation should not be used in the real node, but during development and for testing other functionality
pub (crate) mod in_memory_utxo {
    use std::collections::HashMap;
    use std::fmt::{Display, Formatter, Result as FmtResult};
    use std::error::Error;

    use tokio::sync::RwLock;
    use crate::address::AccountAddress;
    use crate::transactions::{OutPoint, UTXO};
    use crate::storage::UtxoStorage;

    /// Simple error type for in-memory storage
    #[derive(Debug, Clone)]
    pub enum InMemoryStorageError {
        NotFound(OutPoint),
        Other(String),
    }

    impl Display for InMemoryStorageError {
        fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
            match self {
                InMemoryStorageError::NotFound(op) => {
                    write!(f, "UTXO not found for outpoint: {:?}", op)
                }
                InMemoryStorageError::Other(msg) => write!(f, "{msg}"),
            }
        }
    }

    impl Error for InMemoryStorageError {}

    /// Thread-safe in-memory UTXO storage using an RwLock<HashMap<OutPoint, UTXO>>.
    /// Multiple readers can acquire read-locks simultaneously, but only one writer at a time.
    pub struct InMemoryUtxoStorage {
        inner: RwLock<HashMap<OutPoint, UTXO>>,
    }

    impl InMemoryUtxoStorage {
        /// Creates a new empty storage.
        pub fn new() -> Self {
            Self {
                inner: RwLock::new(HashMap::new()),
            }
        }
    }

    impl Default for InMemoryUtxoStorage {
        fn default() -> Self {
            Self::new()
        }
    }

    impl UtxoStorage for InMemoryUtxoStorage {
        type StorageError = InMemoryStorageError;

        async fn get_utxos_for_address(
            &self,
            address: &AccountAddress
        ) -> Result<Vec<(OutPoint, UTXO)>, Self::StorageError> {
            let read_guard = self.inner.read().await;
            let mut result = Vec::new();
            for (outpoint, utxo) in read_guard.iter() {
                if &utxo.owner == address {
                    // clone to return the data
                    result.push((outpoint.clone(), utxo.clone()));
                }
            }
            Ok(result)
        }

        async fn get_utxo(
            &self,
            outpoint: &OutPoint
        ) -> Result<UTXO, Self::StorageError> {
            let read_guard = self.inner.read().await;
            match read_guard.get(outpoint) {
                Some(utxo) => Ok(utxo.clone()),
                None => Err(InMemoryStorageError::NotFound(outpoint.clone())),
            }
        }

        async fn put_utxo(
            &mut self,
            outpoint: &OutPoint,
            utxo: UTXO
        ) -> Result<(), Self::StorageError> {
            let mut write_guard = self.inner.write().await;
            write_guard.insert(outpoint.clone(), utxo);
            Ok(())
        }

        async fn remove_utxo(
            &mut self,
            outpoint: &OutPoint
        ) -> Result<(), Self::StorageError> {
            let mut write_guard = self.inner.write().await;
            let removed = write_guard.remove(outpoint);
            if removed.is_none() {
                return Err(InMemoryStorageError::NotFound(outpoint.clone()));
            }
            Ok(())
        }

        async fn batch_put_utxos(
            &mut self,
            utxos: Vec<(OutPoint, UTXO)>
        ) -> Result<(), Self::StorageError> {
            let mut write_guard = self.inner.write().await;
            for (op, u) in utxos {
                write_guard.insert(op, u);
            }
            Ok(())
        }

        async fn batch_remove_utxos(
            &mut self,
            outpoints: Vec<OutPoint>
        ) -> Result<(), Self::StorageError> {
            let mut write_guard = self.inner.write().await;
            for op in outpoints {
                let removed = write_guard.remove(&op);
                if removed.is_none() {
                    return Err(InMemoryStorageError::NotFound(op));
                }
            }
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::transactions::{OutPoint, UTXO, TransactionHash};
        use crate::address::AccountAddress;

        #[tokio::test]
        async fn test_in_memory_utxo_storage_thread_safety() {
            let mut storage = InMemoryUtxoStorage::default();

            let outpoint = OutPoint {
                txid: TransactionHash::new(&[1u8; 32]),
                vout: 0,
            };
            let utxo = UTXO {
                txid: outpoint.txid,
                vout: outpoint.vout,
                value: 100,
                owner: AccountAddress::new(&[9u8; 20]),
            };

            storage.put_utxo(&outpoint, utxo.clone()).await.unwrap();

            let fetched = storage.get_utxo(&outpoint).await.unwrap();
            assert_eq!(fetched.value, 100);

            let all_for_owner = storage
                .get_utxos_for_address(&utxo.owner)
                .await
                .unwrap();
            assert_eq!(all_for_owner.len(), 1);

            storage.remove_utxo(&outpoint).await.unwrap();
            assert!(storage.get_utxo(&outpoint).await.is_err());
        }
    }

}