use crate::utxo::encode_utxo_key;
use crate::{METADATA_KEY, STATE_KEY, WriteView};
use std::collections::{HashMap, HashSet, hash_map::Entry};
use stryi_core::BlockUndo;
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash};
use stryi_core::storage::{
    BlockIndexData, BlockStorage, ChainMetadata, ChainState, StorageError, StorageWrite,
    TransactionIndexData, UtxoStorage, UtxoWrite,
};
use stryi_core::transactions::{OutPoint, UTXO};

impl UtxoWrite for WriteView<'_> {
    fn batch_put_utxos(&mut self, utxos: Vec<(OutPoint, UTXO)>) -> Result<(), StorageError> {
        // Read each address set once from the transaction, then keep changes local.
        // UTXO writes still happen in order so repeated outpoints use the last owner.
        let mut memberships: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();

        for (outpoint, utxo) in utxos {
            if utxo.txid != outpoint.txid || utxo.vout != outpoint.vout {
                return Err(StorageError::InvalidTransition(format!(
                    "UTXO value does not match key {outpoint}"
                )));
            }

            if let Some(previous) = self.get_utxo(outpoint)?
                && previous.owner != utxo.owner
            {
                let membership = match memberships.entry(previous.owner) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => entry.insert(
                        self.get("addresses", &previous.owner.data)?
                            .unwrap_or_default(),
                    ),
                };
                if !membership.remove(&outpoint) {
                    return Err(
                        self.report_corruption(format!("Address index is missing UTXO {outpoint}"))
                    );
                }
            }

            let membership = match memberships.entry(utxo.owner) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    entry.insert(self.get("addresses", &utxo.owner.data)?.unwrap_or_default())
                }
            };
            membership.insert(outpoint);

            self.put("utxo", &encode_utxo_key(&outpoint), &utxo)?;
        }

        for (address, membership) in memberships {
            self.put("addresses", &address.data, &membership)?;
        }

        Ok(())
    }

    fn batch_remove_utxos(&mut self, outpoints: Vec<OutPoint>) -> Result<(), StorageError> {
        let mut memberships: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();

        for outpoint in outpoints {
            let utxo = self
                .get_utxo(outpoint)?
                .ok_or(StorageError::UtxoNotFound(outpoint))?;
            let membership = match memberships.entry(utxo.owner) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    entry.insert(self.get("addresses", &utxo.owner.data)?.unwrap_or_default())
                }
            };
            if !membership.remove(&outpoint) {
                return Err(
                    self.report_corruption(format!("Address index is missing UTXO {outpoint}"))
                );
            }

            self.remove("utxo", &encode_utxo_key(&outpoint))?;
        }

        for (address, membership) in memberships {
            self.put("addresses", &address.data, &membership)?;
        }

        Ok(())
    }
}

impl StorageWrite for WriteView<'_> {
    fn put_block(&mut self, block: &Block, index: &BlockIndexData) -> Result<(), StorageError> {
        let hash = block.block_hash();
        if let Some(saved) = self.get_block_by_hash(hash)?
            && saved != *block
        {
            return Err(self.report_corruption(format!("Different block bodies share hash {hash}")));
        }

        if let Some(saved) = self.get_block_index(&hash)?
            && saved != *index
        {
            return Err(
                self.report_corruption(format!("Branch work index for block {hash} changed"))
            );
        }

        if let Some(saved) = self.get_block_hash_by_height(block.header.height)?
            && saved != hash
        {
            return Err(StorageError::InvalidTransition(format!(
                "Height {} is already canonical",
                block.header.height
            )));
        }

        self.put("blocks", &hash.data, block)?;
        self.put("block_indexes", &hash.data, index)?;
        self.reader
            .put_cf(
                self.inner.table("heights"),
                block.header.height.to_be_bytes(),
                hash.data,
            )
            .map_err(|error| crate::error::backend_error("write heights", error))?;

        for (position, transaction) in block.data.transactions.iter().enumerate() {
            let tx_index = u32::try_from(position).map_err(|_| {
                StorageError::InvalidTransition("Transaction index exceeds u32".into())
            })?;
            let location = TransactionIndexData {
                block_hash: hash,
                block_height: block.header.height,
                tx_index,
            };

            self.put(
                "transaction_indexes",
                &transaction.data.hash().data,
                &location,
            )?;
        }

        Ok(())
    }

    fn remove_canonical_block(&mut self, block: &Block) -> Result<(), StorageError> {
        // Remove only canonical height/tx indexes; block body, work index and undo stay by hash
        let block_hash = block.block_hash();
        if self.get_block_hash_by_height(block.header.height)? != Some(block_hash) {
            return Err(self.report_corruption(format!(
                "Canonical mapping for block {block_hash} disagrees"
            )));
        }

        self.remove("heights", &block.header.height.to_be_bytes())?;

        for transaction in &block.data.transactions {
            let hash = transaction.data.hash();

            // Shared tx might already point to another canonical block, don't erase it
            let Some(location): Option<TransactionIndexData> =
                self.get("transaction_indexes", &hash.data)?
            else {
                continue;
            };
            if location.block_hash != block_hash {
                self.get_transaction(&hash)?;
                continue;
            }

            let indexed = block.data.transactions.get(location.tx_index as usize);
            if location.block_height != block.header.height
                || indexed.is_none_or(|transaction| transaction.data.hash() != hash)
            {
                return Err(self.report_corruption(format!(
                    "Transaction {hash} does not match detached block {} index {}",
                    location.block_hash, location.tx_index
                )));
            }

            self.remove("transaction_indexes", &hash.data)?;
        }

        Ok(())
    }

    fn put_block_undo(&mut self, hash: BlockHash, undo: BlockUndo) -> Result<(), StorageError> {
        self.put("undo", &hash.data, &undo)
    }

    fn put_chain_state(&mut self, state: &ChainState) -> Result<(), StorageError> {
        self.put("stats", &STATE_KEY, state)
    }

    fn put_metadata(&mut self, metadata: &ChainMetadata) -> Result<(), StorageError> {
        self.put("metadata", METADATA_KEY, metadata)
    }
}
