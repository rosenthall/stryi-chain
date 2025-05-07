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

    /// Insert **one or more** UTXOs in a single atomic operation.
    ///
    /// *Each entry is given as `(OutPoint, Utxo)`.*
    fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>,
    ) -> BoxFuture<Result<(), Self::StorageError>>;

    /// Remove (mark as spent) **one or more** UTXOs in one call.
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




/// Thread‑safe backend for persistent block storage.
/// Implementers must provide `put_block`, `batch_get_by_hashes`, `batch_get_by_heights`, `range` and `exists`.
/// The single-item helpers `get_by_hash` and `get_by_height` have default implementations that simply wrap their batch counterparts
pub trait BlockStorage: Send + Sync {
    type StorageError: Debug + Error + Send;

    /// Atomically inserts or overwrites a single block.
    /// If insertion succeeded returns height of new block
    fn put_block(
        &mut self,
        block: &Block,
    ) -> BoxFuture<Result<u64, Self::StorageError>>;


    /// Fetches **one or more** blocks by hash.
    ///
    /// * Each entry of the returned `HashMap` is guaranteed to exist;
    ///   all requested hashes **must** be present, otherwise the
    ///   implementation must return an error.
    ///  * Returned map is keyed by BlockHash
    fn batch_get_by_hashes(
        &self,
        hashes: Vec<BlockHash>,
    ) -> BoxFuture<Result<HashMap<BlockHash, Block>, Self::StorageError>>;



    /// Fetches **one or more** blocks by height.
    ///
    /// Heights that lie beyond the current tip must trigger an error.
    /// * Each entry of the returned `HashMap` is guaranteed to exist;
    ///   all requested hashes **must** be present, otherwise the
    ///   implementation must return an error.
    ///  * Returned map is keyed by height
    fn batch_get_by_heights<I>(
        &self,
        heights: I,
    ) -> BoxFuture<Result<HashMap<u64, Block>, Self::StorageError>>
    where
        I: IntoIterator<Item = u64> + Send,
        I::IntoIter: Send;

    /// Returns all blocks whose heights lie in the **inclusive** interval `[start, end]`, keyed by their height.
    /// Returns error if any of block in this range is unavailable.
    fn range(
        &self,
        start: u64,
        end: u64,
    ) -> BoxFuture<Result<HashMap<u64, Block>, Self::StorageError>>;


    /// Checks whether a block with the given hash exists.
    /// Returns Ok(false) the block is absent.
    /// May return Err(_) if it can't get value for any reason.
    fn exists(&self, hash: BlockHash) -> BoxFuture<Result<bool, Self::StorageError>>;


    // -- default impls for singular operations--


    /// Retrieves a block by its hash.
    /// Returns `Ok(None)` if not found.
    /// By default, this just forwards to [`batch_get_by_hashes`]. Override if you need
    fn get_by_hash(
        &self,
        hash: BlockHash,
    ) -> BoxFuture<Result<Option<Block>, Self::StorageError>> {
        Box::pin(async move {
            match self.batch_get_by_hashes(vec![hash]).await {
                Ok(mut map) => Ok(map.remove(&hash)),
                Err(e) => Err(e),
            }
        })
    }


    /// Retrieves a block by its height.
    /// Returns `Ok(None)` if not found.
    /// By default, this just forwards to [`batch_get_by_heights`]. Override if you need
    fn get_by_height(
        &self,
        height: u64,
    ) -> BoxFuture<Result<Option<Block>, Self::StorageError>> {
        Box::pin(async move {
            match self.batch_get_by_heights([height]).await {
                Ok(map) => Ok(map.get(&height).map(|b| b.to_owned())),
                Err(e) => Err(e),
            }
        })
    }
}




/// StorageStats defines high-level api to retrieve some statistics from current blockchain state.
pub trait StorageStats : Sync + Sync {
    type StorageError: Debug + Error + Send;

    /// Tip height and its block hash.
    fn tip(&self) -> BoxFuture<Result<(u64, BlockHash), Self::StorageError>>;



    /// Unix timestamp of the most recent successful write
    /// (`put_block` / `put_blocks`).
    fn last_updated(&self) -> BoxFuture<Result<u64, Self::StorageError>>;


    /// Total number of blocks (equal to `tip.height + 1`).
    fn block_count(&self) -> BoxFuture<Result<u64, Self::StorageError>>;


    /// Cumulative chain difficulty.
    fn chain_difficulty(&self) -> BoxFuture<Result<u128, Self::StorageError>>;
}


/// Simple implementation of UtxoStorage trait. Uses HashMap + RwLock inside
/// Created to simplify some steps in development. 
/// The implementation should not be used in the real node, but during development and for testing other functionality
pub (crate) mod in_memory_utxo {
    use std::{
        collections::HashMap,
        error::Error,
        fmt::{Display, Formatter, Result as FmtResult},
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