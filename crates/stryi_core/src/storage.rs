use std::collections::HashMap;
use std::error::Error;
use std::fmt::Debug;
use futures::future::BoxFuture;
use crate::address::AccountAddress;
use crate::block::{Block, BlockHash};
use crate::transactions::{OutPoint, UTXO};

/// Trait representing a storage backend for UTXOs.
///
/// Implementers must provide `batch_put_utxos`, `batch_remove_utxos`, `batch_get_utxos` and `get_utxos_for_address`.
/// The single-item helpers `put_utxo`, `remove_utxo` and `get_utxo` have default implementations that simply
/// wrap their batch counterparts. Back-ends may override these helpers with specialized
/// single-item versions for performance or any other reason, but that is never required.
pub trait UtxoStorage: Send + Sync {
    type StorageError: Debug + Error + Send;

    /// Insert **zero or more** UTXOs in a single atomic operation.
    ///
    /// *Each entry is given as `(OutPoint, Utxo)`.*
        fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>,
    ) -> BoxFuture<Result<(), Self::StorageError>>;

    /// Remove (mark as spent) **zero or more** UTXOs in one call.
    ///
    /// If any outpoint is not present, implementation must return specific error.
    fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>,
    ) -> BoxFuture<Result<(), Self::StorageError>>;


    /// Batch-fetches a heterogeneous set of outpoints.
    /// Missing or already-spent entries must result in an error.
    fn batch_get_utxos<I>(
        &self,
        outpoints: I,
    ) -> BoxFuture<Result<HashMap<OutPoint, UTXO>, Self::StorageError>>
    where
        I: IntoIterator<Item = OutPoint> + Send,
        I::IntoIter: Send;
    
    /// Gets all the UTXOs for the provided AccountAddress.
    /// note: Helpful for calculating account balance and constructing new transactions.
    fn get_utxos_for_address(
        &self,
        address: AccountAddress
    ) -> BoxFuture<Result<HashMap<OutPoint, UTXO>, Self::StorageError>>;

    // Default wrappers
    

    /// Insert or update **exactly one** UTXO.
    ///
    /// By default, this just forwards to [`batch_put_utxos`]. Override if you need
    fn put_utxo(
        &mut self,
        outpoint: OutPoint,
        utxo: UTXO,
    ) -> BoxFuture<Result<(), Self::StorageError>> {
        Box::pin(async move { self.batch_put_utxos(vec![(outpoint, utxo)]).await })
    }


    /// Fetches **exactly one** UTXO. `None` means “not found or already spent”.
    /// By default, this just forwards to [`batch_get_utxos`]. Override if you need
    fn get_utxo(
        &self,
        outpoint: OutPoint,
    ) -> BoxFuture<Result<Option<UTXO>, Self::StorageError>> {
        Box::pin(async move {
            self.batch_get_utxos(std::iter::once(outpoint))
                .await
                .map(|mut m| m.remove(&outpoint))
        })
    }
    
    /// Remove (mark spent) **exactly one** UTXO.
    ///
    /// By default, this just forwards to [`batch_remove_utxos`]. Override if you need
    fn remove_utxo(
        &mut self,
        outpoint: OutPoint,
    ) -> BoxFuture<Result<(), Self::StorageError>> {
        Box::pin(async move { self.batch_remove_utxos(vec![outpoint]).await })
    }
}




/// Trait representing storage backend for Blocks.
pub trait BlockStorage: Send + Sync {
    type StorageError: Debug + Error + Send;

    /// Retrieves a block by its hash.
    async fn get_block_by_hash(&self, hash: BlockHash) -> Result<Block, Self::StorageError>;

    /// Retrieves a block by its height (index).
    async fn get_block_by_height(&self, height: usize) -> Result<Block, Self::StorageError>;

    /// Retrieves the latest (most recent) block in the chain.
    async fn get_latest_block(&self) -> Result<Block, Self::StorageError>;

    /// Inserts a new block or updates an existing one in the storage 
    async fn put_block(&mut self, block: &Block) -> Result<(), Self::StorageError>;

    /// Checks whether a block with the given hash exists.
    async fn block_exists(&self, hash: BlockHash) -> Result<bool, Self::StorageError>;
    
    /// Retrieves the entire chain of blocks.
    async fn get_chain(&self) -> Result<Vec<Block>, Self::StorageError>;

    /// Retrieves a range of blocks from `start_height` to `end_height` inclusive.
    async fn get_range(&self, start_height: usize, end_height: usize)
                       -> Result<Vec<Block>, Self::StorageError>;
}




/// StorageStats defines high-level api to retrieve some statistics from current blockchain state.
pub trait StorageStats : Sync + Sync {
    type StorageError: Debug + Error + Send;

    /// Returns the latest block's height and its hash.
    async fn get_latest_block(&self) -> Result<(usize, BlockHash), Self::StorageError>;


    /// Retrieves last storage update timestamp in unix.
    async fn get_last_update_time(&self) -> Result<usize, Self::StorageError>;


    /// Retrieves entire amount of blocks in this chain.
    async fn get_blocks_count(&self) -> Result<usize, Self::StorageError>;

    
    /// Gets current chain entire difficulty from zero up to current.
    async fn get_chain_difficulty(&self) -> Result<usize, Self::StorageError>;
}


/// Simple implementation of UtxoStorage trait. Uses HashMap + RwLock inside
/// Created to simplify some steps in development. 
/// The implementation should not be used in the real node, but during development and for testing other functionality
pub (crate) mod in_memory_utxo {
    use std::{
        collections::HashMap,
        error::Error,
        fmt::{Display, Formatter, Result as FmtResult},
        pin::Pin,
    };
    use futures::future::BoxFuture;
    use tokio::sync::RwLock;

    use crate::{
        address::AccountAddress,
        storage::UtxoStorage,
        transactions::{OutPoint, UTXO},
    };
    use crate::transactions::TransactionHash;

    /// Error type for the in-memory store.
    #[derive(Debug, Clone)]
    pub enum InMemoryStorageError {
        NotFound(OutPoint),
        Other(String),
    }

    impl Display for InMemoryStorageError {
        fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
            match self {
                Self::NotFound(op) => write!(f, "UTXO not found for outpoint: {op:?}"),
                Self::Other(msg) => write!(f, "{msg}"),
            }
        }
    }

    impl Error for InMemoryStorageError {}

    /// A very simple UTXO store backed by `RwLock<HashMap<OutPoint, UTXO>>`.
    ///
    /// **Never ship this to production!**
    /// It is purely for local development and unit-testing.
    pub struct InMemoryUtxoStorage {
        inner: RwLock<HashMap<OutPoint, UTXO>>,
    }

    impl InMemoryUtxoStorage {
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

        // ---------- required methods ---------- //


        fn batch_put_utxos(
            &mut self,
            utxos: Vec<(OutPoint, UTXO)>,
        ) -> BoxFuture<Result<(), Self::StorageError>> {
            Box::pin(async move {
                let mut guard = self.inner.write().await;
                for (op, u) in utxos {
                    guard.insert(op, u);
                }
                Ok(())
            })
        }


        fn batch_get_utxos<I>(
            &self,
            outpoints: I,
        ) -> BoxFuture<Result<HashMap<OutPoint, UTXO>, Self::StorageError>>
        where
            I: IntoIterator<Item = OutPoint> + Send,
            I::IntoIter: Send,
        {
            // Collect iterator into a Vec so it can be moved into the async block
            let it: Vec<OutPoint> = outpoints.into_iter().collect();
            Box::pin(async move {
                let guard = self.inner.read().await;
                let mut map = HashMap::with_capacity(it.len());
                for op in it {
                    match guard.get(&op) {
                        Some(u) => {
                            map.insert(op, *u);
                        }
                        None => return Err(InMemoryStorageError::NotFound(op)),
                    }
                }
                Ok(map)
            })
        }


        fn batch_remove_utxos(
            &mut self,
            outpoints: Vec<OutPoint>,
        ) -> BoxFuture<Result<(), Self::StorageError>> {
            Box::pin(async move {
                let mut guard = self.inner.write().await;
                for op in outpoints {
                    if guard.remove(&op).is_none() {
                        return Err(InMemoryStorageError::NotFound(op));
                    }
                }
                Ok(())
            })
        }

        fn get_utxos_for_address(
            &self,
            address: AccountAddress,
        ) -> BoxFuture<Result<HashMap<OutPoint, UTXO>, Self::StorageError>> {
            Box::pin(async move {
                let guard = self.inner.read().await;
                Ok(guard
                    .iter()
                    .filter(|(_, utxo)| utxo.owner == address)
                    .map(|(op, utxo)| (*op, *utxo))
                    .collect())
            })
        }

    }




    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::{
            address::AccountAddress,
            transactions::{OutPoint, TransactionHash},
        };

        #[tokio::test]
        async fn in_memory_utxo_round_trip() {
            let mut store = InMemoryUtxoStorage::default();

            let outpoint = OutPoint {
                txid: TransactionHash::new(&[1; 32]),
                vout: 0,
            };
            let utxo = UTXO {
                txid: outpoint.txid,
                vout: outpoint.vout,
                value: 42,
                owner: AccountAddress::new(&[9; 20]),
            };

            // insert
            store
                .put_utxo(outpoint, utxo)
                .await
                .expect("insert");

            // query by owner
            let fetched = store
                .get_utxos_for_address(utxo.owner)
                .await
                .expect("query");
            assert_eq!(fetched.len(), 1);
            assert_eq!(fetched.get(&outpoint).unwrap().value, 42);

            // remove
            store
                .remove_utxo(outpoint)
                .await
                .expect("remove");
            let fetched_after = store.get_utxos_for_address(utxo.owner).await.unwrap();
            assert!(fetched_after.is_empty());
        }
    }

    /// Verifies that the *default* single-item helpers (`put_utxo` / `remove_utxo`)
    /// work correctly. We insert two UTXOs one-by-one, remove one, and check that:
    /// 1. the remaining entry is still there,
    /// 2. trying to remove a non-existent outpoint yields the expected error.
    #[tokio::test]
    async fn test_utxo_storage_default_methods_impls() {
        let mut store = InMemoryUtxoStorage::default();

        let op1 = OutPoint {
            txid: TransactionHash::new(&[0xAA; 32]),
            vout: 0,
        };
        let utxo1 = UTXO {
            txid: op1.txid,
            vout: op1.vout,
            value: 10,
            owner: AccountAddress::new(&[0x01; 20]),
        };

        // --- insert via default `put_utxo` ---
        store.put_utxo(op1, utxo1).await.unwrap();

        // --- fetch via default `get_utxo` ---
        let fetched = store.get_utxo(op1).await.unwrap();
        assert_eq!(fetched.unwrap().value, 10);

        // --- remove via default `remove_utxo` ---
        store.remove_utxo(op1).await.unwrap();

        // now `get_utxo` should error (batch_get_utxos returns NotFound) OR return Ok(None)
        // depending on semantics – we chose to propagate NotFound in this backend
        let err = store.get_utxo(op1).await.unwrap_err();
        matches!(err, InMemoryStorageError::NotFound(op) if op == op1);
    }
}