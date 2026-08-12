use crate::{StryiStorage, StryiStorageError};
use fjall::Slice;
use serde::{Deserialize, Serialize};
use stryi_core::block::BlockHash;
use stryi_core::storage::BlockStorage;
use stryi_core::transactions::TransactionHash;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransactionIndexData {
    pub block_hash: BlockHash,
    pub block_height: u64,
    pub tx_index: u32,
}

impl StryiStorage {
    pub fn get_transaction_index(
        &self,
        tx_hash: &TransactionHash,
    ) -> Result<Option<TransactionIndexData>, StryiStorageError> {
        let raw = self
            .transaction_index_partition
            .get(Slice::from(&tx_hash.data[..]))
            .map_err(StryiStorageError::FjallError)?;

        match raw {
            Some(bytes) => {
                let decoded = postcard::from_bytes::<TransactionIndexData>(&bytes)
                    .map_err(StryiStorageError::DeserializationError)?;
                Ok(Some(decoded))
            }
            None => Ok(None),
        }
    }

    pub fn remove_transaction_index(
        &self,
        tx_hash: &TransactionHash,
    ) -> Result<(), StryiStorageError> {
        self.transaction_index_partition
            .remove(Slice::from(&tx_hash.data[..]))
            .map_err(StryiStorageError::FjallError)?;
        Ok(())
    }

    pub async fn prune_confirmed_transaction_indexes_for_blocks<I>(
        &mut self,
        block_hashes: I,
    ) -> Result<(), StryiStorageError>
    where
        I: IntoIterator<Item = BlockHash>,
    {
        let block_hashes: Vec<BlockHash> = block_hashes.into_iter().collect();
        if block_hashes.is_empty() {
            return Ok(());
        }

        let blocks = self
            .batch_get_blocks_by_hashes(block_hashes.clone())
            .await?;
        let mut batch = self.db.batch();

        for block_hash in block_hashes {
            let Some(block) = blocks.get(&block_hash) else {
                continue;
            };

            for transaction in &block.data.transactions {
                let tx_hash = transaction.data.hash();
                let Some(index) = self.get_transaction_index(&tx_hash)? else {
                    continue;
                };

                if index.block_hash == block_hash {
                    batch.remove(
                        &self.transaction_index_partition,
                        Slice::from(&tx_hash.data[..]),
                    );
                }
            }
        }

        batch.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GenesisInitConfig;
    use crate::blocks::tests::create_test_storage;
    use stryi_core::address::AccountAddress;
    use stryi_core::block::Block;
    use stryi_core::transactions::{Transaction, TransactionData, TransactionKind, TransactionOut};

    fn make_unsigned_payment(value: u64, recipient_seed: u8) -> Transaction {
        Transaction::new_unsigned(TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![],
            outputs: vec![TransactionOut {
                value,
                recipient: AccountAddress::new(&[recipient_seed; 20]),
            }],
        })
    }

    #[tokio::test]
    async fn indexes_genesis_transaction() -> Result<(), StryiStorageError> {
        let temp_dir = tempfile::TempDir::new().expect("temp dir");
        let storage = StryiStorage::initialize_in_path(
            temp_dir.path().to_path_buf(),
            Some(GenesisInitConfig::new_test()),
        )
        .await?;

        let genesis = storage
            .get_block_by_height(0)
            .await?
            .expect("genesis block exists");
        let tx_hash = genesis.data.transactions[0].data.hash();
        let index = storage
            .get_transaction_index(&tx_hash)?
            .expect("genesis tx index exists");

        assert_eq!(index.block_hash, genesis.block_hash());
        assert_eq!(index.block_height, 0);
        assert_eq!(index.tx_index, 0);

        Ok(())
    }

    #[tokio::test]
    async fn prune_keeps_new_canonical_mapping_for_shared_tx_hash() -> Result<(), StryiStorageError>
    {
        let (mut storage, _temp_dir) = create_test_storage(true);

        let shared_tx = make_unsigned_payment(10, 1);
        let stale_tx = make_unsigned_payment(20, 2);
        let fresh_tx = make_unsigned_payment(30, 3);

        let old_block = Block::new(
            vec![shared_tx.clone(), stale_tx.clone()],
            BlockHash::empty(),
            1,
            1,
            1_700_000_010,
            1,
        );
        let new_block = Block::new(
            vec![shared_tx.clone(), fresh_tx.clone()],
            BlockHash::empty(),
            1,
            1,
            1_700_000_020,
            1,
        );

        storage.put_block(&old_block).await?;
        storage.put_block(&new_block).await?;
        storage
            .prune_confirmed_transaction_indexes_for_blocks([old_block.block_hash()])
            .await?;

        let shared_index = storage
            .get_transaction_index(&shared_tx.data.hash())?
            .expect("shared tx index exists");
        assert_eq!(shared_index.block_hash, new_block.block_hash());

        assert_eq!(storage.get_transaction_index(&stale_tx.data.hash())?, None);

        let fresh_index = storage
            .get_transaction_index(&fresh_tx.data.hash())?
            .expect("fresh tx index exists");
        assert_eq!(fresh_index.block_hash, new_block.block_hash());

        Ok(())
    }
}
