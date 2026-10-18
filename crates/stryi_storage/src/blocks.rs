use crate::{ReadSource, StorageView};
use stryi_core::block::{Block, BlockHash};
use stryi_core::storage::{
    BlockIndexData, BlockStorage, ConfirmedTransaction, StorageError, TransactionIndexData,
    verify_genesis_work_shape,
};
use stryi_core::transactions::TransactionHash;

impl<R: ReadSource> BlockStorage for StorageView<'_, R> {
    fn get_block_hash_by_height(&self, height: u64) -> Result<Option<BlockHash>, StorageError> {
        let Some(bytes) = self.raw_get("heights", &height.to_be_bytes())? else {
            return Ok(None);
        };

        let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
            self.report_corruption(format!("Invalid block hash at height {height}"))
        })?;
        let hash = BlockHash::try_from(bytes.as_slice()).map_err(|error| {
            self.report_corruption(format!("Invalid block hash at height {height}: {error}"))
        })?;

        Ok(Some(hash))
    }

    fn get_block_by_hash(&self, hash: BlockHash) -> Result<Option<Block>, StorageError> {
        let Some(block): Option<Block> = self.get("blocks", &hash.data)? else {
            return Ok(None);
        };

        verify_genesis_work_shape(&block)
            .map_err(|error| self.report_corruption(error.to_string()))?;
        if block.block_hash() != hash {
            return Err(self.report_corruption(format!("Block body does not match key {hash}")));
        }

        Ok(Some(block))
    }

    fn get_block_by_height(&self, height: u64) -> Result<Option<Block>, StorageError> {
        let Some(hash) = self.get_block_hash_by_height(height)? else {
            return Ok(None);
        };

        let block = self.get_block_by_hash(hash)?.ok_or_else(|| {
            self.report_corruption(format!("Height {height} references absent block {hash}"))
        })?;
        if block.header.height != height {
            return Err(self.report_corruption(format!(
                "Height {height} references a block at height {}",
                block.header.height
            )));
        }

        Ok(Some(block))
    }

    fn get_block_index(&self, hash: &BlockHash) -> Result<Option<BlockIndexData>, StorageError> {
        self.get("block_indexes", &hash.data)
    }

    fn get_transaction(
        &self,
        hash: &TransactionHash,
    ) -> Result<Option<ConfirmedTransaction>, StorageError> {
        let Some(location): Option<TransactionIndexData> =
            self.get("transaction_indexes", &hash.data)?
        else {
            return Ok(None);
        };

        // A locator is only useful when its retained body agrees. Do not require a
        // canonical height mapping: detachment removes it before reading locators.
        let block = self
            .get_block_by_hash(location.block_hash)?
            .ok_or_else(|| {
                self.report_corruption(format!(
                    "Transaction {hash} references absent block {}",
                    location.block_hash
                ))
            })?;
        if block.header.height != location.block_height {
            return Err(self.report_corruption(format!(
                "Transaction {hash} height {} disagrees with block {} height {}",
                location.block_height, location.block_hash, block.header.height
            )));
        }

        let transaction = block
            .data
            .transactions
            .into_iter()
            .nth(location.tx_index as usize)
            .filter(|transaction| transaction.data.hash() == *hash)
            .ok_or_else(|| {
                self.report_corruption(format!(
                    "Transaction {hash} does not match block {} index {}",
                    location.block_hash, location.tx_index
                ))
            })?;

        Ok(Some(ConfirmedTransaction {
            transaction,
            location,
        }))
    }
}
