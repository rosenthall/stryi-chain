use std::collections::{HashMap, HashSet};

use bincode::config::standard;
use fjall::Slice;

use crate::error::StryiStorageError;
use crate::StryiStorage;

use stryi_core::address::AccountAddress;
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::{OutPoint, UTXO};

/// Helper function that encodes an `OutPoint` into a 36-byte key.
///
/// Layout:
/// - `[0..32]` => `txid.data` (the raw 32 bytes of the transaction hash)
/// - `[32..36]` => `vout` in big-endian bytes
fn encode_utxo_key(outpoint: &OutPoint) -> [u8; 36] {
    let mut key = [0u8; 36];
    key[..32].copy_from_slice(&outpoint.txid.data);
    key[32..36].copy_from_slice(&outpoint.vout.to_be_bytes());
    key
}

/// Implements `UtxoStorage` for `StryiStorage` using a transactional Fjall partition.
impl UtxoStorage for StryiStorage {
    type StorageError = StryiStorageError;

    /// Scans all UTXOs in the partition and filters those owned by `address`.
    ///
    /// **Note**: This is a full scan. It opens a read transaction and iterates
    /// over every key in `utxo_partition`, checking for matching `owner`.
    /// This can be slow for large databases.
    async fn get_utxos_for_address(
        &self,
        address: &AccountAddress
    ) -> Result<Vec<(OutPoint, UTXO)>, Self::StorageError> {
        let mut results = Vec::new();

        // Open a read transaction so we can iterate over keys in a consistent snapshot.
        let read_tx = self.keyspace.read_tx();

        // Use prefix with an empty slice to scan every key in the utxo_partition.
        // This returns an iterator of Result<(UserKey, UserValue), fjall::Error>.
        let kv_iter = read_tx.prefix(&self.utxo_partition, &[]);

        // Iterate over each key/value pair, filtering by address.
        for kv_item in kv_iter {
            let (key_bytes, value_bytes) = kv_item.map_err(StryiStorageError::FjallError)?;

            // Deserialize the UTXO from the stored bytes.
            let (utxo, _) : (UTXO, _)= bincode::serde::decode_from_slice(&value_bytes, standard())
                .map_err(StryiStorageError::DeserializationError)?;

            // Check if the UTXO's owner matches the requested address.
            if utxo.owner == *address {
                // We also verify that the key is 36 bytes (32 for txid, 4 for vout).
                if key_bytes.len() == 36 {
                    let mut vout_arr = [0u8; 4];
                    vout_arr.copy_from_slice(&key_bytes[32..36]);
                    let vout = u32::from_be_bytes(vout_arr);

                    // Build the OutPoint from the key's vout and the UTXO's txid.
                    let outpoint = OutPoint {
                        txid: utxo.txid.clone(),
                        vout,
                    };
                    results.push((outpoint, utxo));
                }
            }
        }

        Ok(results)
    }

    /// Retrieves a single UTXO by its `OutPoint`.
    ///
    /// - Returns `Ok(UTXO)` if found, or `Err(NotFound)` if it doesn't exist.
    async fn get_utxo(
        &self,
        outpoint: &OutPoint
    ) -> Result<UTXO, Self::StorageError> {
        let key = encode_utxo_key(outpoint);

        // We can read from the transactional partition handle directly for a single key lookup.
        let raw_opt = self.utxo_partition
            .get(Slice::from(&key))
            .map_err(StryiStorageError::FjallError)?;

        let raw = match raw_opt {
            Some(bytes) => bytes,
            None => {
                return Err(StryiStorageError::NotFound(
                    format!("UTXO not found for outpoint: {:?}", outpoint)
                ))
            }
        };

        // Deserialize the UTXO
        let (utxo, _) = bincode::serde::decode_from_slice(&raw, standard())
            .map_err(StryiStorageError::DeserializationError)?;

        Ok(utxo)
    }

    /// Retrieves multiple UTXOs for a set of `OutPoint`s.
    ///
    /// This repeatedly calls `get_utxo` for each `OutPoint`.
    async fn get_utxos(
        &self,
        outpoints: &HashSet<OutPoint>
    ) -> Result<HashMap<OutPoint, UTXO>, Self::StorageError> {
        let mut results = HashMap::new();

        for op in outpoints {
            let utxo = self.get_utxo(op).await?;
            results.insert(op.clone(), utxo);
        }
        Ok(results)
    }

    /// Inserts or updates a single UTXO.
    ///
    /// Uses a `write_tx()` to ensure atomicity. If serialization fails or the commit fails,
    /// the database remains unchanged.
    async fn put_utxo(
        &mut self,
        outpoint: &OutPoint,
        utxo: UTXO
    ) -> Result<(), Self::StorageError> {
        let key = encode_utxo_key(outpoint);
        let encoded_utxo = bincode::serde::encode_to_vec(&utxo, standard())
            .map_err(StryiStorageError::SerializationError)?;

        // Start a write transaction
        let mut tx = self.keyspace.write_tx();

        // Insert the key/value
        tx.insert(&self.utxo_partition, Slice::from(&key), Slice::from(encoded_utxo));

        // Commit the changes
        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }

    /// Removes a single UTXO if present. Returns `NotFound` if the UTXO does not exist.
    async fn remove_utxo(
        &mut self,
        outpoint: &OutPoint
    ) -> Result<(), Self::StorageError> {
        // If we want a `NotFound` error if there's no such UTXO, check first.
        let _ = self.get_utxo(outpoint).await?;

        let key = encode_utxo_key(outpoint);

        let mut tx = self.keyspace.write_tx();
        tx.remove(&self.utxo_partition, Slice::from(&key));
        
        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }

    /// Batch-inserts multiple UTXOs in a single transaction.
    ///
    /// If any step fails, the transaction is not committed, so no partial writes occur.
    async fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>
    ) -> Result<(), Self::StorageError> {
        let mut tx = self.keyspace.write_tx();

        for (op, utxo) in utxos {
            let key = encode_utxo_key(&op);
            let encoded = bincode::serde::encode_to_vec(&utxo, standard())
                .map_err(StryiStorageError::SerializationError)?;
            tx.insert(&self.utxo_partition, Slice::from(&key), Slice::from(encoded));
        }

        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }

    /// Batch-removes multiple UTXOs in a single transaction.
    ///
    /// If any UTXO doesn't exist, we return `NotFound` and no changes are committed.
    async fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>
    ) -> Result<(), Self::StorageError> {
        let mut tx = self.keyspace.write_tx();

        for op in outpoints {
            // Check existence
            let _ = self.get_utxo(&op).await?;
            let key = encode_utxo_key(&op);

            tx.remove(&self.utxo_partition, Slice::from(&key)) ;
        }

        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }
}
