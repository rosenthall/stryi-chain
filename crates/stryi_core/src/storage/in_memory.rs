//! A very simple implementation of the domain storage traits.
//! Uses HashMaps with cloned read views and staged writes under a mutex.
//! Created to simplify some steps in development, especially in testing.
//! Should not be used in the real node, but during development and for testing other functionality.

use super::{
    BlockIndexData, BlockStorage, ChainMetadata, ChainState, ChainTransition,
    ChainTransitionResult, ConfirmedTransaction, FullNodeStorage, StorageError, StorageReader,
    StorageStats, StorageWrite, TransactionIndexData, UndoStorage, UtxoStorage, UtxoWrite,
    apply_transition,
};
use crate::BlockUndo;
use crate::address::AccountAddress;
use crate::block::{Block, BlockHash};
use crate::transactions::{OutPoint, TransactionHash, UTXO};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct MemoryView {
    utxos: HashMap<OutPoint, UTXO>,
    blocks: HashMap<BlockHash, Block>,
    heights: HashMap<u64, BlockHash>,
    indexes: HashMap<BlockHash, BlockIndexData>,
    transactions: HashMap<TransactionHash, TransactionIndexData>,
    undos: HashMap<BlockHash, BlockUndo>,
    state: Option<ChainState>,
    metadata: Option<ChainMetadata>,
}

/// A minimalistic, in-ram implementation of storage backed by std HashMap instances.
///
/// **Never use in a real node**
/// It is purely for local development and unit-testing.
#[derive(Clone, Default)]
pub struct StryiInMemoryStorage {
    data: Arc<Mutex<MemoryView>>,
}

impl StryiInMemoryStorage {
    /// Initialize StryiInMemoryStorage.
    /// Takes genesis for proper initial state; panics if it is invalid.
    pub fn new(genesis: Block) -> Self {
        let storage = Self::default();
        let metadata = ChainMetadata {
            // In-memory fixtures have no persistent schema.
            schema_version: 0,
            chain_name: "test".into(),
            protocol_version: u64::from(genesis.header.version),
            created_unix_ts: 0,
        };

        storage
            .apply_transition(ChainTransition::Genesis {
                block: &genesis,
                metadata: &metadata,
            })
            .expect("Valid test genesis");

        storage
    }

    pub(crate) fn update<F, T>(&self, callback: F) -> Result<T, StorageError>
    where
        F: FnOnce(&mut MemoryView) -> Result<T, StorageError>,
    {
        let mut guard = self.data.lock().map_err(|_| StorageError::Unhealthy)?;
        let mut pending = guard.clone();
        let result = callback(&mut pending)?;

        *guard = pending;

        Ok(result)
    }
}

impl StorageReader for StryiInMemoryStorage {
    type Read<'a> = MemoryView;

    fn read(&self) -> Result<Self::Read<'_>, StorageError> {
        self.data
            .lock()
            .map(|data| data.clone())
            .map_err(|_| StorageError::Unhealthy)
    }
}

impl FullNodeStorage for StryiInMemoryStorage {
    fn apply_transition(
        &self,
        transition: ChainTransition<'_>,
    ) -> Result<ChainTransitionResult, StorageError> {
        self.update(|pending| apply_transition(pending, transition))
    }
}

impl UtxoStorage for MemoryView {
    fn get_utxo(&self, outpoint: OutPoint) -> Result<Option<UTXO>, StorageError> {
        Ok(self.utxos.get(&outpoint).copied())
    }

    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> Result<HashMap<OutPoint, UTXO>, StorageError> {
        Ok(self
            .utxos
            .iter()
            .filter(|(_, utxo)| utxo.owner == address)
            .map(|(outpoint, utxo)| (*outpoint, *utxo))
            .collect())
    }
}

impl BlockStorage for MemoryView {
    fn get_block_hash_by_height(&self, height: u64) -> Result<Option<BlockHash>, StorageError> {
        Ok(self.heights.get(&height).copied())
    }

    fn get_block_by_hash(&self, hash: BlockHash) -> Result<Option<Block>, StorageError> {
        Ok(self.blocks.get(&hash).cloned())
    }

    fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, StorageError> {
        match self.heights.get(&height) {
            None => Ok(None),
            Some(hash) => self.blocks.get(hash).cloned().map(Some).ok_or_else(|| {
                StorageError::Corruption(format!("Height {height} references missing block {hash}"))
            }),
        }
    }

    fn get_block_index(&self, hash: &BlockHash) -> Result<Option<BlockIndexData>, StorageError> {
        Ok(self.indexes.get(hash).cloned())
    }

    fn get_transaction(
        &self,
        hash: &TransactionHash,
    ) -> Result<Option<ConfirmedTransaction>, StorageError> {
        let Some(location) = self.transactions.get(hash) else {
            return Ok(None);
        };

        let block = self.blocks.get(&location.block_hash).ok_or_else(|| {
            StorageError::Corruption(format!(
                "Transaction {hash} references absent block {}",
                location.block_hash
            ))
        })?;
        let transaction = block
            .data
            .transactions
            .get(location.tx_index as usize)
            .filter(|transaction| {
                block.block_hash() == location.block_hash
                    && block.header.height == location.block_height
                    && transaction.data.hash() == *hash
            })
            .ok_or_else(|| {
                StorageError::Corruption(format!(
                    "Transaction {hash} does not match block {} index {}",
                    location.block_hash, location.tx_index
                ))
            })?;

        Ok(Some(ConfirmedTransaction {
            transaction: transaction.clone(),
            location: location.clone(),
        }))
    }
}

impl StorageStats for MemoryView {
    fn chain_state(&self) -> Result<Option<ChainState>, StorageError> {
        Ok(self.state.clone())
    }

    fn metadata(&self) -> Result<Option<ChainMetadata>, StorageError> {
        Ok(self.metadata.clone())
    }
}

impl UndoStorage for MemoryView {
    fn get_block_undo(&self, hash: BlockHash) -> Result<Option<BlockUndo>, StorageError> {
        Ok(self.undos.get(&hash).cloned())
    }
}

impl UtxoWrite for MemoryView {
    fn batch_put_utxos(&mut self, utxos: Vec<(OutPoint, UTXO)>) -> Result<(), StorageError> {
        self.utxos.extend(utxos);
        Ok(())
    }

    fn batch_remove_utxos(&mut self, outpoints: Vec<OutPoint>) -> Result<(), StorageError> {
        for outpoint in &outpoints {
            if !self.utxos.contains_key(outpoint) {
                return Err(StorageError::UtxoNotFound(*outpoint));
            }
        }

        for outpoint in outpoints {
            self.utxos.remove(&outpoint);
        }

        Ok(())
    }
}

impl StorageWrite for MemoryView {
    fn put_block(&mut self, block: &Block, index: &BlockIndexData) -> Result<(), StorageError> {
        let hash = block.block_hash();
        self.blocks.insert(hash, block.clone());
        self.heights.insert(block.header.height, hash);
        self.indexes.insert(hash, index.clone());

        for (position, transaction) in block.data.transactions.iter().enumerate() {
            let tx_index = u32::try_from(position).map_err(|_| {
                StorageError::InvalidTransition("Transaction index overflow".into())
            })?;

            self.transactions.insert(
                transaction.data.hash(),
                TransactionIndexData {
                    block_hash: hash,
                    block_height: block.header.height,
                    tx_index,
                },
            );
        }

        Ok(())
    }

    fn remove_canonical_block(&mut self, block: &Block) -> Result<(), StorageError> {
        let hash = block.block_hash();

        if self.heights.get(&block.header.height) != Some(&hash) {
            return Err(StorageError::Corruption(
                "Detached block is outside canonical heights".into(),
            ));
        }

        self.heights.remove(&block.header.height);
        for transaction in &block.data.transactions {
            let txid = transaction.data.hash();
            if self
                .transactions
                .get(&txid)
                .is_some_and(|index| index.block_hash == hash)
            {
                self.transactions.remove(&txid);
            }
        }

        Ok(())
    }

    fn put_block_undo(&mut self, hash: BlockHash, undo: BlockUndo) -> Result<(), StorageError> {
        self.undos.insert(hash, undo);
        Ok(())
    }

    fn put_chain_state(&mut self, state: &ChainState) -> Result<(), StorageError> {
        self.state = Some(state.clone());
        Ok(())
    }

    fn put_metadata(&mut self, metadata: &ChainMetadata) -> Result<(), StorageError> {
        self.metadata = Some(metadata.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::GenesisState;
    use crate::difficulty::{ChainWork, HashThreshold};
    use crate::{
        address::AccountAddress,
        block::{Block, BlockData, BlockHash, BlockHeader},
        storage::{BlockStorage, StorageError, StorageStats, UtxoStorage},
        transactions::{
            OutPoint, Transaction, TransactionData, TransactionHash, TransactionKind,
            TransactionOut, UTXO,
        },
    };
    use tokio::task;

    // Test helper: deterministic genesis block
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
                merkle_root_hash: Block::compute_merkle_root(std::slice::from_ref(&genesis_tx)),
                previous_block_hash: BlockHash::empty(),
                height: 0,
                hash_threshold: HashThreshold::ZERO,
                timestamp: 0,
                nonce: 0,
                genesis_state: Some(GenesisState::default()),
            },
            data: BlockData {
                transactions: vec![genesis_tx],
            },
        }
    }

    #[test]
    fn in_memory_storage_single_utxo_round_trip() {
        let mut store = StryiInMemoryStorage::new(make_test_genesis_block())
            .read()
            .unwrap();

        let op = OutPoint {
            txid: TransactionHash::new(&[1; 32]),
            vout: 0,
        };
        let owner = AccountAddress::new(&[0xAA; 20]);
        let utxo = UTXO {
            txid: op.txid,
            vout: op.vout,
            value: 42,
            owner,
        };

        store.batch_put_utxos(vec![(op, utxo)]).expect("insert");
        let fetched = store.get_utxos_for_address(owner).unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[&op].value, 42);

        store.batch_remove_utxos(vec![op]).unwrap();
        assert!(store.get_utxos_for_address(owner).unwrap().is_empty());
    }

    #[test]
    fn in_memory_storage_helper_methods_and_errors() {
        let mut store = StryiInMemoryStorage::new(make_test_genesis_block())
            .read()
            .unwrap();

        // insert two distinct UTXOs
        let op1 = OutPoint {
            txid: TransactionHash::new(&[0xAB; 32]),
            vout: 0,
        };
        let op2 = OutPoint {
            txid: TransactionHash::new(&[0xCD; 32]),
            vout: 1,
        };
        let addr1 = AccountAddress::new(&[0x01; 20]);
        let addr2 = AccountAddress::new(&[0x02; 20]);
        let utxo1 = UTXO {
            txid: op1.txid,
            vout: op1.vout,
            value: 10,
            owner: addr1,
        };
        let utxo2 = UTXO {
            txid: op2.txid,
            vout: op2.vout,
            value: 20,
            owner: addr2,
        };

        store.batch_put_utxos(vec![(op1, utxo1)]).unwrap();
        store.batch_put_utxos(vec![(op2, utxo2)]).unwrap();
        assert_eq!(store.get_utxo(op1).unwrap().unwrap().value, 10);

        // remove first - should succeed
        store.batch_remove_utxos(vec![op1]).unwrap();

        // subsequent fetch must error
        let err = store.batch_get_utxos([op1]).unwrap_err();
        assert!(matches!(err, StorageError::UtxoNotFound(o) if o == op1));

        // second UTXO untouched
        assert_eq!(store.get_utxo(op2).unwrap().unwrap().value, 20);
    }

    #[test]
    fn in_memory_storage_batch_insert_and_remove() {
        let mut store = StryiInMemoryStorage::new(make_test_genesis_block())
            .read()
            .unwrap();

        let batch: Vec<(OutPoint, UTXO)> = (0..100)
            .map(|i| {
                let op = OutPoint {
                    txid: TransactionHash::new(&[i as u8; 32]),
                    vout: 0,
                };
                let u = UTXO {
                    txid: op.txid,
                    vout: 0,
                    value: i as u64,
                    owner: AccountAddress::new(&[i as u8; 20]),
                };
                (op, u)
            })
            .collect();

        store.batch_put_utxos(batch.clone()).unwrap();

        // remove even-indexed outpoints
        let to_remove: Vec<_> = batch
            .iter()
            .enumerate()
            .filter_map(|(idx, (op, _))| if idx % 2 == 0 { Some(*op) } else { None })
            .collect();
        store.batch_remove_utxos(to_remove.clone()).unwrap();

        for (idx, (op, _)) in batch.iter().enumerate() {
            match store.get_utxo(*op) {
                // present -> must be an odd index
                Ok(Some(_)) if idx % 2 == 1 => {}
                // removed -> backend may return Err(UtxoNotFound)
                Ok(None) | Err(StorageError::UtxoNotFound(_)) if idx % 2 == 0 => {}
                // anything else is a failure
                other => panic!("unexpected result for idx {idx}: {other:?}"),
            }
        }
    }

    #[test]
    fn in_memory_storage_block_storage_and_stats() {
        let storage = StryiInMemoryStorage::new(make_test_genesis_block());
        let before = storage.read().unwrap();

        // build & store a dummy block at height 1
        let addr = AccountAddress::new(&[0xFF; 20]);
        let coinbase_tx = Transaction {
            data: TransactionData {
                version: 1,
                kind: TransactionKind::Coinbase {
                    height: 1,
                    parent: BlockHash::empty(),
                },
                inputs: vec![],
                outputs: vec![TransactionOut {
                    value: 50,
                    recipient: addr,
                }],
            },
            signature: Default::default(),
        };
        let block1 = Block {
            header: BlockHeader {
                version: 1,
                merkle_root_hash: Block::compute_merkle_root(std::slice::from_ref(&coinbase_tx)),
                previous_block_hash: BlockHash::empty(),
                height: 1,
                hash_threshold: HashThreshold::MAX >> 1,
                timestamp: 1_700_000_010,
                nonce: 0,
                genesis_state: None,
            },
            data: BlockData {
                transactions: vec![coinbase_tx],
            },
        };

        storage
            .apply_transition(ChainTransition::Append { block: &block1 })
            .unwrap();

        let store = storage.read().unwrap();
        assert_eq!(before.tip().unwrap().0, 0);
        assert!(before.get_block_by_height(1).unwrap().is_none());

        // fetch by hash / height / range
        assert!(
            store
                .batch_get_blocks_by_hashes(vec![block1.block_hash()])
                .unwrap()
                .contains_key(&block1.block_hash())
        );
        assert!(
            store
                .batch_get_blocks_by_heights(vec![1])
                .unwrap()
                .contains_key(&1)
        );

        let range = 0u64..=1u64;

        assert_eq!(store.blocks_range(range).unwrap().len(), 2);

        // stats
        assert_eq!(store.tip().unwrap().0, 1);
        assert_eq!(store.block_count().unwrap(), 2);
        // Genesis contributes zero work; only the normal block adds its work.
        assert_eq!(store.chain_work().unwrap(), ChainWork::from(2u64));

        let outpoint = OutPoint {
            txid: TransactionHash::new(&[9; 32]),
            vout: 0,
        };
        let error = storage
            .update(|pending| {
                pending.batch_put_utxos(vec![(
                    outpoint,
                    UTXO {
                        txid: outpoint.txid,
                        vout: 0,
                        value: 42,
                        owner: addr,
                    },
                )])?;

                Err::<(), _>(StorageError::InvalidTransition("abort fixture".into()))
            })
            .unwrap_err();
        assert!(matches!(error, StorageError::InvalidTransition(_)));

        let after = storage.read().unwrap();
        assert!(after.get_utxo(outpoint).unwrap().is_none());
        assert_eq!(after.chain_state().unwrap(), store.chain_state().unwrap());
        assert_eq!(
            after.get_utxos_for_address(addr).unwrap(),
            store.get_utxos_for_address(addr).unwrap()
        );
    }

    #[tokio::test]
    async fn in_memory_storage_concurrent_access() {
        let store = StryiInMemoryStorage::new(make_test_genesis_block());

        const TASKS: usize = 8;
        const OPS_PER_TASK: usize = 50;

        let mut handles = Vec::new();
        for t in 0..TASKS {
            let store = store.clone();
            handles.push(task::spawn(async move {
                for i in 0..OPS_PER_TASK {
                    let idx = (t * OPS_PER_TASK + i) as u8;
                    let op = OutPoint {
                        txid: TransactionHash::new(&[idx; 32]),
                        vout: 0,
                    };
                    let u = UTXO {
                        txid: op.txid,
                        vout: 0,
                        value: idx as u64,
                        owner: AccountAddress::new(&[idx; 20]),
                    };

                    store
                        .update(|pending| {
                            pending.batch_put_utxos(vec![(op, u)])?;
                            pending.batch_remove_utxos(vec![op])
                        })
                        .unwrap();
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        // after all tasks, storage should still contain only genesis outputs
        let guard = store.read().unwrap();
        assert_eq!(guard.block_count().unwrap(), 1); // only genesis
    }
}
