use stryi_core::block::{Block, BlockHash};
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use crate::actor::proxy::StryiStorageProxy;
use crate::actor::StryiStorageActorMessage::{BatchPutUtxos, BatchRemoveUtxos, BlockExists, GetBlockByHash, GetBlockByHeight, GetChain, GetRange, GetUtxo, GetUtxos, GetUtxosForAddress, PutBlock, PutUtxo, RemoveUtxo};
use crate::StryiStorageError;

/// --- BLOCK STORAGE TRAIT IMPL ---
impl BlockStorage for StryiStorageProxy {
    type StorageError = StryiStorageError;


    /// Returns a block by its hash.
    async fn get_block_by_hash(&self, hash: BlockHash) -> Result<Block, Self::StorageError> {
        self.dispatch(|tx| GetBlockByHash { hash, resp: tx }).await
    }

    /// Returns a block by its height.
    async fn get_block_by_height(&self, height: usize) -> Result<Block, Self::StorageError> {
        self.dispatch(|tx| GetBlockByHeight { height, resp: tx }).await
    }

    /// Returns the latest block.
    async fn get_latest_block(&self) -> Result<Block, Self::StorageError> {
        todo!("Remove this from this trait, we already have one in StorageStats")
    }

    /// Inserts a new block (or updates an existing one).
    async fn put_block(&mut self, block: &Block) -> Result<(), Self::StorageError> {
        self.dispatch(|tx| PutBlock { block: block.clone(), resp: tx }).await
    }

    /// Checks whether a block exists by its hash.
    async fn block_exists(&self, hash: BlockHash) -> Result<bool, Self::StorageError> {
        self.dispatch(|tx| BlockExists { hash, resp: tx }).await
    }

    /// Returns the entire chain of blocks.
    async fn get_chain(&self) -> Result<Vec<Block>, Self::StorageError> {
        self.dispatch(|tx| GetChain { resp: tx }).await
    }

    /// Returns a range of blocks.
    async fn get_range(&self, start_height: usize, end_height: usize) -> Result<Vec<Block>, Self::StorageError> {
        self.dispatch(|tx| GetRange { start_height, end_height, resp: tx }).await
    }
}


impl UtxoStorage for StryiStorageProxy {
    type StorageError = StryiStorageError;

    async fn get_utxos_for_address(&self, address: &stryi_core::address::AccountAddress) -> Result<Vec<(stryi_core::transactions::OutPoint, stryi_core::transactions::UTXO)>, Self::StorageError> {
        self.dispatch(|tx| GetUtxosForAddress { address: address.clone(), resp: tx }).await
    }

    async fn get_utxo(&self, outpoint: &stryi_core::transactions::OutPoint) -> Result<stryi_core::transactions::UTXO, Self::StorageError> {
        self.dispatch(|tx| GetUtxo { outpoint: outpoint.clone(), resp: tx }).await
    }

    async fn get_utxos(&self, outpoints: &std::collections::HashSet<stryi_core::transactions::OutPoint>) -> Result<std::collections::HashMap<stryi_core::transactions::OutPoint, stryi_core::transactions::UTXO>, Self::StorageError> {
        self.dispatch(|tx| GetUtxos { outpoints: outpoints.iter().cloned().collect(), resp: tx }).await
    }

    async fn put_utxo(&mut self, outpoint: &stryi_core::transactions::OutPoint, utxo: stryi_core::transactions::UTXO) -> Result<(), Self::StorageError> {
        self.dispatch(|tx| PutUtxo { outpoint: outpoint.clone(), utxo, resp: tx }).await
    }

    async fn remove_utxo(&mut self, outpoint: &stryi_core::transactions::OutPoint) -> Result<(), Self::StorageError> {
        self.dispatch(|tx| RemoveUtxo { outpoint: outpoint.clone(), resp: tx }).await
    }

    async fn batch_put_utxos(&mut self, utxos: Vec<(stryi_core::transactions::OutPoint, stryi_core::transactions::UTXO)>) -> Result<(), Self::StorageError> {
        self.dispatch(|tx| BatchPutUtxos { utxos, resp: tx }).await
    }

    async fn batch_remove_utxos(&mut self, outpoints: Vec<stryi_core::transactions::OutPoint>) -> Result<(), Self::StorageError> {
        self.dispatch(|tx| BatchRemoveUtxos { outpoints, resp: tx }).await
    }
}






/// --- STORAGE STATS TRAIT IMPL ---

impl StorageStats for StryiStorageProxy {
    type StorageError = StryiStorageError;

    async fn get_latest_block(&self) -> Result<(usize, BlockHash), Self::StorageError> {
        let stats = self.get_storage_stats().await?;
        Ok(stats.latest_block)
    }

    async fn get_last_update_time(&self) -> Result<usize, Self::StorageError> {
        let stats = self.get_storage_stats().await?;
        Ok(stats.last_update_time)
    }

    async fn get_blocks_count(&self) -> Result<usize, Self::StorageError> {
        let stats = self.get_storage_stats().await?;
        Ok(stats.blocks_count)
    }

    async fn get_chain_difficulty(&self) -> Result<usize, Self::StorageError> {
        let stats = self.get_storage_stats().await?;
        Ok(stats.chain_difficulty)
    }
}
