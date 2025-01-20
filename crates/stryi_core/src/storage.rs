use crate::address::AccountAddress;
use crate::block::{Block, BlockHash};
use crate::transactions::{OutPoint, UTXO};

/// Trait representing storage backend for UTXOs.
pub trait UtxoStorage {
    type StorageError: std::fmt::Debug + std::error::Error + Clone + Send;
    
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
pub trait BlockStorage {
    type StorageError: std::fmt::Debug + std::error::Error + Clone + Send;

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