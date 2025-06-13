//! A very simple implementation of UtxoStorage, BlockStorage, and StorageStats traits
//! Uses a lot of HashMaps + RwLocks inside.
//! Created to simplify some steps in development, especially in testing
//! Should not be used in the real node, but during development and for testing other functionality

use futures::future::BoxFuture;
use std::collections::HashMap;
use std::future::ready;
use std::range::RangeInclusive;
use tokio::sync::RwLock;
use crate::storage::{BlockStorage, RangeError, StorageStats, UndoStorage, UtxoStorage};
use crate::transactions::TransactionKind;
use crate::{address::AccountAddress, transactions::{OutPoint, UTXO}, BlockUndo};
use crate::block::{Block, BlockHash};
use crate::error::StryiCoreError;
use thiserror::Error;

/// Fine-grained errors returned by in-memory storage.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum InMemoryStorageError {
    // UTXO layer
    #[error("UTXO {0:?} not found")]
    UtxoNotFound(OutPoint),

    #[error("UTXO {0:?} already exists")]
    UtxoExists(OutPoint),

    // Block layer
    #[error("Block {0:?} not found")]
    BlockNotFound(BlockHash),

    #[error("No block at height {0}")]
    HeightMissing(u64),

    #[error("Invalid range: start={start}, end={end}")]
    InvalidRange { start: usize, end: usize },

    // Misc / plumbing
    #[error("Undo data failed: {0}")]
    UndoFailure(String),

    #[error("{0}")]
    Other(String),
}

impl From<RangeError> for InMemoryStorageError {
    fn from(r: RangeError) -> Self {
        match r {
            RangeError::InvalidRange { start, end } => Self::InvalidRange { start, end },
        }
    }
}

impl From<StryiCoreError> for InMemoryStorageError {
    fn from(e: StryiCoreError) -> Self {
        Self::Other(e.to_string())
    }
}

#[derive(Clone, PartialEq)]
struct StryiInMemoryStorageState {
    pub latest_block : (u64, BlockHash),
    pub last_update_time : u64,
    pub blocks_count : u64,
    pub chain_difficulty:  u128,
 }

impl Default for StryiInMemoryStorageState {
    fn default() -> Self {
        Self {
            latest_block: (0, BlockHash::empty()),
            last_update_time: 0,
            blocks_count: 1,
            chain_difficulty: 0,
        }
    }
}


impl StryiInMemoryStorageState {

    /// Setups new StryiInMemoryStorageStats based on provided genesis block
    pub fn new_from_genesis(block : &Block) -> Self {
        assert!(block.header.is_genesis);
        StryiInMemoryStorageState {
            latest_block : (0, block.block_hash()),
            last_update_time : block.header.timestamp,
            blocks_count: 1,
            chain_difficulty: 1u128 << block.header.difficulty_bits,
        }
    }
}


/// A very simple implementation of Repository (UtxoStorage, BlockStorage, StorageStats traits)
/// Implementation is backed by std HashMap instances
///
/// **Never use in a real node**
/// It is purely for local development and unit-testing.
#[derive(Default)]
pub struct StryiInMemoryStorage {
    utxos: RwLock<HashMap<OutPoint, UTXO>>,
    blocks : RwLock<HashMap<BlockHash, Block>>,
    blocks_heights : RwLock<HashMap<u64, BlockHash>>,
    blocks_undo : RwLock<HashMap<BlockHash, BlockUndo>>,
    current_state : RwLock<StryiInMemoryStorageState>
}


impl StryiInMemoryStorage {

    /// Initialize StryiInMemoryStorage
    /// Requires providing genesis_block to properly setup state
    /// Panics if provided block isn't proper genesis (see is_genesis flag)
    pub fn new(genesis_block: Block) -> Self {

        assert!(genesis_block.header.is_genesis, "Non-genesis block was provided for initialization of StryiInMemoryStorage");

        let hash = genesis_block.block_hash();

        let state = StryiInMemoryStorageState::new_from_genesis(&genesis_block);


        // setup block_by_* tables
        let mut blocks_by_hash = HashMap::new();
        blocks_by_hash.insert(hash, genesis_block.clone());
        let mut blocks_by_height = HashMap::new();
        blocks_by_height.insert(0, hash);


        // Setup utxo table
        let mut genesis_utxos = HashMap::new();


        let genesis_tx = &genesis_block.data.transactions[0];
        assert!(genesis_tx.data.kind.eq(&TransactionKind::Genesis), "Cannot initialize StryiInMemoryStorage. Provided genesis block's first tx is not TransactionKind::Genesis");

        let genesis_tx_hash = genesis_tx.data.hash();


        // add all the required transactions from genesis tx in utxo table.
        for (vout, out) in genesis_block.data.transactions[0].data.outputs.iter().enumerate() {

            let outpoint = OutPoint {
                txid: genesis_tx_hash,
                vout: vout as u32,
            };

            let utxo = UTXO {
                txid: genesis_tx_hash,
                vout: vout as u32,
                value: out.value,
                owner: out.recipient,
            };

            genesis_utxos.insert(outpoint, utxo);
        }


        Self {
            utxos: RwLock::new(genesis_utxos),
            blocks: RwLock::new(blocks_by_hash),
            blocks_heights: RwLock::new(blocks_by_height),
            blocks_undo: Default::default(), // No BlockUndo entries are required for now
            current_state: RwLock::new(state)
        }
    }
}



// ---- UTXO STORAGE IMPLEMENTATION ----
impl UtxoStorage for StryiInMemoryStorage {
    type StorageError = InMemoryStorageError;

    // ---------- required methods ---------- //

    fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>,
    ) -> BoxFuture<Result<(), Self::StorageError>> {
        Box::pin(async move {
            let mut guard = self.utxos.write().await;
            for (op, u) in utxos {
                guard.insert(op, u);
            }
            Ok(())
        })
    }

    fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>,
    ) -> BoxFuture<Result<(), Self::StorageError>> {
        Box::pin(async move {
            let mut guard = self.utxos.write().await;
            for op in outpoints {
                if guard.remove(&op).is_none() {
                    return Err(InMemoryStorageError::UtxoNotFound(op));
                }
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
            let guard = self.utxos.read().await;
            let mut map = HashMap::with_capacity(it.len());
            for op in it {
                match guard.get(&op) {
                    Some(u) => {
                        map.insert(op, *u);
                    }
                    None => return Err(InMemoryStorageError::UtxoNotFound(op)),
                }
            }
            Ok(map)
        })
    }

    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> BoxFuture<Result<HashMap<OutPoint, UTXO>, Self::StorageError>> {
        Box::pin(async move {
            let guard = self.utxos.read().await;
            Ok(guard
                .iter()
                .filter(|(_, utxo)| utxo.owner == address)
                .map(|(op, utxo)| (*op, *utxo))
                .collect())
        })
    }
}

// ---- BLOCK STORAGE IMPLEMENTATION ----
impl BlockStorage for StryiInMemoryStorage {
    type StorageError = InMemoryStorageError;

    fn put_block(&mut self, block: &Block) -> BoxFuture<Result<(), Self::StorageError>> {
        let block = block.clone();

        // Compute the block hash from the block
        let block_hash   = block.block_hash();
        let block_height = block.header.height;
        let block_bits   = block.header.difficulty_bits;

        Box::pin(async move {
            // hash->block map
            let mut blocks_map   = self.blocks.write().await;
            // height->hash map
            let mut heights_map  = self.blocks_heights.write().await;

            // Map hash -> block
            blocks_map.insert(block_hash, block.to_owned());
            println!("[test-storage] Mapped block to {}", block_hash);

            // height -> hash
            heights_map.insert(block_height, block_hash);
            println!("[test-storage] Mapped height {} to block {}", block_height, block_hash);

            {
                let mut st = self.current_state.write().await;
                st.latest_block     = (block_height, block_hash);
                st.last_update_time = block.header.timestamp;
                st.blocks_count    += 1;
                st.chain_difficulty = st.chain_difficulty.wrapping_add(1u128 << block_bits);

                // diagnostic log
                println!(
                    "[test-storage] ↑ tip -> height={}, hash={}, total_blocks={}, chain_difficulty={}",
                    st.latest_block.0,
                    st.latest_block.1,
                    st.blocks_count,
                    st.chain_difficulty,
                );
            }

            Ok(())
        })
    }


    fn batch_get_blocks_by_hashes(
        &self,
        hashes: Vec<BlockHash>,
    ) -> BoxFuture<Result<HashMap<BlockHash, Block>, Self::StorageError>> {
        // capture a reference to the map so it can be used inside the async closure
        let blocks_lock = &self.blocks;
        Box::pin(async move {
            // acquire a read guard
            let guard = blocks_lock.read().await;
            let mut result = HashMap::with_capacity(hashes.len());

            // for each requested hash, fetch or return an error
            for h in hashes {
                match guard.get(&h) {
                    Some(block) => { result.insert(h, block.clone()); }
                    None => { return Err(InMemoryStorageError::BlockNotFound(h)); }
                }
}

            Ok(result)
        })
    }


    fn batch_get_blocks_by_heights<I>(
        &self,
        heights: I,
    ) -> BoxFuture<Result<HashMap<u64, Block>, Self::StorageError>>
    where
        I: IntoIterator<Item = u64> + Send + Clone,
        I::IntoIter: Send,
    {
        // collect heights up front so we can move them into the async block
        let heights_vec: Vec<u64> = heights.into_iter().collect();

        Box::pin(async move {
            // first read the height -> hash map
            let heights_map = self.blocks_heights.read().await;
            // then read the hash -> block map
            let blocks_map = self.blocks.read().await;

            let mut result = HashMap::with_capacity(heights_vec.len());
            for height in heights_vec {
                // lookup the block hash at this height
                let hash = heights_map.get(&height)
                    .ok_or(InMemoryStorageError::HeightMissing(height))?;

                // lookup the block by hash
                let block = blocks_map.get(hash)
                    .ok_or(InMemoryStorageError::BlockNotFound(*hash))?;

                result.insert(height, block.clone());
            }
            Ok(result)
        })
    }

    fn blocks_range(
        &self,
        range: RangeInclusive<usize>,
    ) -> BoxFuture<Result<HashMap<u64, Block>, Self::StorageError>> {
        // Capture locks so they can be used inside async move
        let heights_lock = &self.blocks_heights;
        let blocks_lock  = &self.blocks;
        let start = range.start;
        let end   = range.end;

        Box::pin(async move {

            Self::validate_range(range)?;

            // Read both maps under their own RwLock guards
            let heights_map = heights_lock.read().await;
            let blocks_map  = blocks_lock.read().await;

            let mut result = HashMap::with_capacity(end - start + 1);
            for h in start..=end {
                let height = h as u64;
                // 1) lookup the block hash at this height
                let hash = heights_map.get(&height)
                    .ok_or_else(|| InMemoryStorageError::Other(
                        format!("No block at height {}", height)
                    ))?;
                // 2) lookup the actual block by hash
                let block = blocks_map.get(hash)
                    .ok_or_else(|| InMemoryStorageError::Other(
                        format!("Block not found for hash {:?}", hash)
                    ))?;
                result.insert(height, block.clone());
            }

            Ok(result)
        })
    }

    fn block_exists(&self, hash: BlockHash) -> BoxFuture<Result<bool, Self::StorageError>> {
        Box::pin(async move {
            Ok(ready(self.blocks.read().await.get(&hash).is_some()).await)
        })
    }
}

// ---- STORAGE STATS IMPLEMENTATION ----
impl StorageStats for StryiInMemoryStorage {
    type StorageError = InMemoryStorageError;

    fn tip(&self) -> BoxFuture<Result<(u64, BlockHash), Self::StorageError>> {
        Box::pin(async move {
            let st = self.current_state.read().await;
            Ok(st.latest_block)
        })
    }

    fn last_updated(&self) -> BoxFuture<Result<u64, Self::StorageError>> {
        Box::pin(async move {
            let st = self.current_state.read().await;
            Ok(st.last_update_time)
        })
    }

    fn block_count(&self) -> BoxFuture<Result<u64, Self::StorageError>> {
        Box::pin(async move {
            let st = self.current_state.read().await;
            Ok(st.blocks_count)
        })
    }

    fn chain_difficulty(&self) -> BoxFuture<Result<u128, Self::StorageError>> {
        Box::pin(async move {
            let st = self.current_state.read().await;
            Ok(st.chain_difficulty)
        })
    }
}


// ---- UNDO STORAGE IMPLEMENTATION ----
impl UndoStorage for StryiInMemoryStorage {
    type StorageError = InMemoryStorageError;

    fn put_block_undo(
        &self,
        hash: BlockHash,
        undo: BlockUndo,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        Box::pin(async move {
            let mut guard = self.blocks_undo.write().await;
            guard.insert(hash, undo);
            Ok(())
        })
    }

    fn get_block_undo(
        &self,
        hash: BlockHash,
    ) -> BoxFuture<'_, Result<Option<BlockUndo>, Self::StorageError>> {
        Box::pin(async move {
            let guard = self.blocks_undo.read().await;
            Ok(guard.get(&hash).cloned())
        })
    }

    fn delete_block_undo(
        &self,
        hash: BlockHash,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        Box::pin(async move {
            let mut guard = self.blocks_undo.write().await;
            guard.remove(&hash);
            Ok(())
        })
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        address::AccountAddress,
        block::{Block, BlockData, BlockHeader, BlockHash},
        storage::{BlockStorage, StorageStats, UtxoStorage, InMemoryStorageError},
        transactions::{
            OutPoint, Transaction, TransactionData, TransactionHash, TransactionKind,
            TransactionOut, UTXO,
        },
    };
    use tokio::task;

    // Test‑helper : deterministic genesis block
    fn make_test_genesis_block() -> Block {
        let premine_owner = AccountAddress::new(&[0u8; 20]);
        let genesis_tx = Transaction {
            data: TransactionData {
                version: 1,
                kind: TransactionKind::Genesis,
                inputs: vec![],
                outputs: vec![TransactionOut {
                    value: 1_000_000,
                    recipient: premine_owner,
                }],
            },
            signature: Default::default(),
        };
        Block {
            header: BlockHeader {
                version: 1,
                merkle_root_hash: Block::compute_merkle_root(&[genesis_tx.clone()]),
                previous_block_hash: BlockHash::empty(),
                height: 0,
                difficulty_bits: 1,
                timestamp: 1_700_000_000,
                nonce: 0,
                is_genesis: true,
            },
            data: BlockData {
                transactions: vec![genesis_tx],
            },
        }
    }

    #[tokio::test]
    async fn in_memory_storage_single_utxo_round_trip() {
        let mut store = StryiInMemoryStorage::new(make_test_genesis_block());
        let op = OutPoint { txid: TransactionHash::new(&[1; 32]), vout: 0 };
        let owner = AccountAddress::new(&[0xAA; 20]);
        let utxo = UTXO { txid: op.txid, vout: op.vout, value: 42, owner };

        store.put_utxo(op, utxo).await.expect("insert");
        let fetched = store.get_utxos_for_address(owner).await.unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[&op].value, 42);

        store.remove_utxo(op).await.unwrap();
        assert!(store.get_utxos_for_address(owner).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn in_memory_storage_helper_methods_and_errors() {
        let mut store = StryiInMemoryStorage::new(make_test_genesis_block());

        // insert two distinct UTXOs
        let op1 = OutPoint { txid: TransactionHash::new(&[0xAB; 32]), vout: 0 };
        let op2 = OutPoint { txid: TransactionHash::new(&[0xCD; 32]), vout: 1 };
        let addr1 = AccountAddress::new(&[0x01; 20]);
        let addr2 = AccountAddress::new(&[0x02; 20]);
        let utxo1 = UTXO { txid: op1.txid, vout: op1.vout, value: 10, owner: addr1 };
        let utxo2 = UTXO { txid: op2.txid, vout: op2.vout, value: 20, owner: addr2 };

        store.put_utxo(op1, utxo1).await.unwrap();
        store.put_utxo(op2, utxo2).await.unwrap();
        assert_eq!(store.get_utxo(op1).await.unwrap().unwrap().value, 10);

        // remove first — should succeed
        store.remove_utxo(op1).await.unwrap();

        // subsequent fetch must error
        let err = store.get_utxo(op1).await.unwrap_err();
        assert!(matches!(err, InMemoryStorageError::UtxoNotFound(o) if o == op1));

        // second UTXO untouched
        assert_eq!(store.get_utxo(op2)  .await.unwrap().unwrap().value, 20);
    }

    #[tokio::test]
    async fn in_memory_storage_batch_insert_and_remove() {
        let mut store = StryiInMemoryStorage::new(make_test_genesis_block());
        let batch: Vec<(OutPoint, UTXO)> = (0..100)
            .map(|i| {
                let op = OutPoint { txid: TransactionHash::new(&[i as u8; 32]), vout: 0 };
                let u = UTXO {
                    txid: op.txid,
                    vout: 0,
                    value: i as u64,
                    owner: AccountAddress::new(&[i as u8; 20]),
                };
                (op, u)
            })
            .collect();

        store.batch_put_utxos(batch.clone()).await.unwrap();
        // remove even‑indexed outpoints
        let to_remove: Vec<_> = batch.iter().enumerate().filter_map(|(idx, (op, _))| {
            if idx % 2 == 0 { Some(*op) } else { None }
        }).collect();
        store.batch_remove_utxos(to_remove.clone()).await.unwrap();

        for (idx, (op, _)) in batch.iter().enumerate() {
            match store.get_utxo(*op).await {
                // present -> must be an odd index
                Ok(Some(_)) if idx % 2 == 1 => {}
                // removed -> backend may return Err(UtxoNotFound)
                Ok(None) | Err(InMemoryStorageError::UtxoNotFound(_)) if idx % 2 == 0 => {}
                // anything else is a failure
                other => panic!("unexpected result for idx {idx}: {other:?}"),
            }
        }


    }

    #[tokio::test]
    async fn in_memory_storage_block_storage_and_stats() {
        let mut store = StryiInMemoryStorage::new(make_test_genesis_block());

        // build & store a dummy block at height 1
        let addr = AccountAddress::new(&[0xFF; 20]);
        let coinbase_tx = Transaction {
            data: TransactionData {
                version: 1,
                kind: TransactionKind::Coinbase,
                inputs: vec![],
                outputs: vec![TransactionOut { value: 50, recipient: addr }],
            },
            signature: Default::default(),
        };
        let block1 = Block {
            header: BlockHeader {
                version: 1,
                merkle_root_hash: Block::compute_merkle_root(&[coinbase_tx.clone()]),
                previous_block_hash: BlockHash::empty(),
                height: 1,
                difficulty_bits: 1,
                timestamp: 1_700_000_010,
                nonce: 0,
                is_genesis: false,
            },
            data: BlockData { transactions: vec![coinbase_tx] },
        };
        store.put_block(&block1).await.unwrap();

        // fetch by hash / height / range
        assert!(store.batch_get_blocks_by_hashes(vec![block1.block_hash()]).await.unwrap().contains_key(&block1.block_hash()));
        assert!(store.batch_get_blocks_by_heights(vec![1]).await.unwrap().contains_key(&1));


        let range = RangeInclusive {
            start : 0usize,
            end: 1usize
        };

        assert_eq!(store.blocks_range(range).await.unwrap().len(), 2);

        // stats
        assert_eq!(store.tip().await.unwrap().0, 1);
        assert_eq!(store.block_count().await.unwrap(), 2);
        assert_eq!(store.chain_difficulty().await.unwrap(), 4); // 2 blocks * 2^1
    }

    #[tokio::test]
    async fn in_memory_storage_concurrent_access() {
        let store = StryiInMemoryStorage::new(make_test_genesis_block());
        let store = std::sync::Arc::new(tokio::sync::Mutex::new(store));

        const TASKS: usize = 8;
        const OPS_PER_TASK: usize = 50;

        let mut handles = Vec::new();
        for t in 0..TASKS {
            let store = store.clone();
            handles.push(task::spawn(async move {
                for i in 0..OPS_PER_TASK {
                    let idx = (t * OPS_PER_TASK + i) as u8;
                    let op = OutPoint { txid: TransactionHash::new(&[idx; 32]), vout: 0 };
                    let u = UTXO { txid: op.txid, vout: 0, value: idx as u64, owner: AccountAddress::new(&[idx; 20]) };
                    {
                        let mut guard = store.lock().await;
                        guard.put_utxo(op, u).await.unwrap();
                        guard.remove_utxo(op).await.unwrap();
                    }
                }
            }));
        }

        for h in handles { h.await.unwrap(); }
        // after all tasks, storage should still contain only genesis outputs
        let guard = store.lock().await;
        assert_eq!(guard.block_count().await.unwrap(), 1); // only genesis
    }
}
