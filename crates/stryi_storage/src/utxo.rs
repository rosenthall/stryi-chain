//! This module implements the `UtxoStorage` trait for `StryiStorage`.
//!
//! We use two partitions for our unspent outputs:
//!
//! 1. **utxo_partition**
//!    - Key: 36 bytes `[txid(32 bytes) | vout(4 bytes, big-endian)]`
//!    - Value: bincode(`UTXO`)
//!
//!    This is the main storage for unspent outputs, allowing fast lookups by `(txid,vout)`.
//!
//! 2. **addresses_partition**
//!    - Key: 20 bytes of `AccountAddress`
//!    - Value: bincode(`HashSet<OutPoint>`)
//!
//!    This is an index from address → the set of outpoints that belong to that address. We keep
//!    this index in sync whenever we add or remove a UTXO in the main partition, so that
//!    `get_utxos_for_address` does not require scanning all UTXOs.

use std::collections::{HashMap, HashSet};
use bincode::config::standard;
use fjall::{Slice, WriteTransaction, ReadTransaction};

use crate::error::StryiStorageError;
use crate::StryiStorage;

use stryi_core::address::AccountAddress;
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::{OutPoint, UTXO};

/// Encodes an outpoint into a 36-byte key:
/// - bytes [0..32]: `txid.data`
/// - bytes [32..36]: `vout` as a big-endian 32-bit integer.
/// We use 4 bytes to handle large numbers of outputs if needed.
fn encode_utxo_key(outpoint: &OutPoint) -> [u8; 36] {
    let mut key = [0u8; 36];
    key[..32].copy_from_slice(&outpoint.txid.data);
    key[32..36].copy_from_slice(&outpoint.vout.to_be_bytes());
    key
}

/// Decodes a bincode `HashSet<OutPoint>` from a slice of bytes, returning an empty set if `None`.
fn decode_outpoints_set(bytes: &[u8]) -> Result<HashSet<OutPoint>, StryiStorageError> {
    let (set, _) = bincode::serde::decode_from_slice::<HashSet<OutPoint>, _>(bytes, standard())
        .map_err(StryiStorageError::DeserializationError)?;
    Ok(set)
}

/// Encodes a `HashSet<OutPoint>` into bytes using bincode.
fn encode_outpoints_set(ops: &HashSet<OutPoint>) -> Result<Vec<u8>, StryiStorageError> {
    bincode::serde::encode_to_vec(ops, standard())
        .map_err(StryiStorageError::SerializationError)
}


/// Loads an existing set of outpoints for a given `address` from the partition,
/// using a read-only transaction (`ReadTransaction`).
///
/// - If the address is found, it deserializes the stored data into a `HashSet<OutPoint>`.
/// - If not found, returns an empty set.
///
/// This function is usually called to retrieve the outpoints that belong to a particular
/// address before performing lookups in the main UTXO partition.
fn load_address_set_read(
    read_tx: &ReadTransaction,
    partition: &fjall::TxPartition,
    address: &AccountAddress
) -> Result<HashSet<OutPoint>, StryiStorageError> {
    let data_opt = read_tx
        .get(partition, Slice::from(&address.data[..]))
        .map_err(StryiStorageError::FjallError)?;

    match data_opt {
        Some(slice) => decode_outpoints_set(&slice.to_vec()),
        None => Ok(HashSet::new()),
    }
}

/// Loads an existing set of outpoints for a given `address` from the partition,
/// using a writable transaction (`WriteTransaction`).
///
/// - If the address is found, it deserializes the stored data into a `HashSet<OutPoint>`.
/// - If not found, returns an empty set.
///
/// This function is typically used inside a single transaction that also modifies
/// the UTXO partition. Once the data is loaded, you can modify the set in memory
/// and then store it back via [`store_address_set_write`].
fn load_address_set_write(
    write_tx: &WriteTransaction,
    partition: &fjall::TxPartition,
    address: &AccountAddress
) -> Result<HashSet<OutPoint>, StryiStorageError> {
    let data_opt = write_tx
        .get(partition, Slice::from(&address.data[..]))
        .map_err(StryiStorageError::FjallError)?;

    match data_opt {
        Some(slice) => decode_outpoints_set(&slice.to_vec()),
        None => Ok(HashSet::new()),
    }
}

/// Stores a `HashSet<OutPoint>` associated with `address` in the `addresses_partition`
/// using a mutable write transaction. This is typically used after you've loaded and
/// updated the address set in memory.
///
/// Any changes to this set will be committed only if the entire transaction is committed
/// successfully. If the transaction fails or is aborted, the original data remains intact.
fn store_address_set_write(
    write_tx: &mut WriteTransaction,
    partition: &fjall::TxPartition,
    address: &AccountAddress,
    set: &HashSet<OutPoint>,
) -> Result<(), StryiStorageError> {
    let encoded = encode_outpoints_set(set)?;
    write_tx.insert(partition, Slice::from(&address.data[..]), Slice::from(encoded));
    Ok(())
}



impl UtxoStorage for StryiStorage {
    type StorageError = StryiStorageError;

    /// Returns the UTXOs owned by `address`. Performing these steps: 
    /// 1) Opens a read transaction
    /// 2) Loads the set of outpoints from `addresses_partition`
    /// 3) For each outpoint, calls `get_utxo` to get the full UTXO
    async fn get_utxos_for_address(
        &self,
        address: &AccountAddress
    ) -> Result<Vec<(OutPoint, UTXO)>, Self::StorageError> {
        let read_tx = self.keyspace.read_tx();

        let outpoints_set = load_address_set_read(&read_tx, &self.addresses_partition, address)?;

        let mut results = Vec::with_capacity(outpoints_set.len());
        for op in outpoints_set {
            let utxo = self.get_utxo(&op).await?;
            results.push((op, utxo));
        }

        Ok(results)
    }

    /// Retrieves a single UTXO by (txid+vout) from the main partition.
    async fn get_utxo(
        &self,
        outpoint: &OutPoint
    ) -> Result<UTXO, Self::StorageError> {
        let key = encode_utxo_key(outpoint);
        let raw_opt = self.utxo_partition
            .get(Slice::from(&key))
            .map_err(StryiStorageError::FjallError)?;

        let raw = match raw_opt {
            Some(bytes) => bytes,
            None => {
                return Err(StryiStorageError::NotFound(
                    format!("UTXO not found for outpoint: {:?}", outpoint)
                ));
            }
        };

        let (utxo, _) = bincode::serde::decode_from_slice::<UTXO, _>(&raw, standard())
            .map_err(StryiStorageError::DeserializationError)?;
        Ok(utxo)
    }

    /// Retrieves multiple UTXOs, uses read-transaction inside 
    async fn get_utxos(
        &self,
        outpoints: &HashSet<OutPoint>
    ) -> Result<HashMap<OutPoint, UTXO>, Self::StorageError> {
        // 1) Open one read transaction
        let read_tx = self.keyspace.read_tx();

        let mut result = HashMap::with_capacity(outpoints.len()); // with_capacity is cool here

        // 2) For each outpoint, do a single 'get' inside the same read transaction
        for op in outpoints {
            let key = encode_utxo_key(op);

            let raw_opt = read_tx
                .get(&self.utxo_partition, Slice::from(&key[..]))
                .map_err(StryiStorageError::FjallError)?;

            let raw = match raw_opt {
                Some(bytes) => bytes,
                None => return Err(StryiStorageError::NotFound(
                    format!("UTXO not found for outpoint: {:?}", op)
                )),
            };

            let (utxo, _) = bincode::serde::decode_from_slice::<UTXO, _>(&raw, standard())
                .map_err(StryiStorageError::DeserializationError)?;

            result.insert(op.clone(), utxo);
        }

        // 3) Return the map of outpoints -> UTXO
        Ok(result)
    }

    /// Inserts or updates a single UTXO, updating the address index accordingly.
    async fn put_utxo(
        &mut self,
        outpoint: &OutPoint,
        utxo: UTXO
    ) -> Result<(), Self::StorageError> {
        let key = encode_utxo_key(outpoint);
        let utxo_bytes = bincode::serde::encode_to_vec(&utxo, standard())
            .map_err(StryiStorageError::SerializationError)?;

        let mut tx = self.keyspace.write_tx();
        // Insert into main partition
        tx.insert(&self.utxo_partition, Slice::from(&key), Slice::from(utxo_bytes));

        // Add outpoint to the address set
        let mut address_set = load_address_set_write(&tx, &self.addresses_partition, &utxo.owner)?;
        address_set.insert(outpoint.clone());
        store_address_set_write(&mut tx, &self.addresses_partition, &utxo.owner, &address_set)?;

        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }

    /// Removes a UTXO by outpoint, also removing it from the owner's set in the address index.
    async fn remove_utxo(
        &mut self,
        outpoint: &OutPoint
    ) -> Result<(), Self::StorageError> {
        // Check which address it belongs to
        let utxo = self.get_utxo(outpoint).await?;

        let key = encode_utxo_key(outpoint);
        let mut tx = self.keyspace.write_tx();

        // Remove from main partition
        tx.remove(&self.utxo_partition, Slice::from(&key));
        
        
        // Remove from address set
        let mut address_set = load_address_set_write(&tx, &self.addresses_partition, &utxo.owner)?;
        address_set.remove(outpoint);
        store_address_set_write(&mut tx, &self.addresses_partition, &utxo.owner, &address_set)?;

        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }

    /// Batch-inserts multiple UTXOs in one transaction, updating each address set accordingly.
    async fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>
    ) -> Result<(), Self::StorageError> {
        let mut tx = self.keyspace.write_tx();

        // Collect address changes in memory: address -> new outpoints
        let mut addr_map: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();

        // Insert each UTXO into the main partition
        for (op, u) in &utxos {
            let key = encode_utxo_key(op);
            let bytes = bincode::serde::encode_to_vec(u, standard())
                .map_err(StryiStorageError::SerializationError)?;
            tx.insert(&self.utxo_partition, Slice::from(&key), Slice::from(bytes));

            addr_map.entry(u.owner.clone())
                .or_insert_with(HashSet::new)
                .insert(op.clone());
        }

        // Update each address set
        for (addr, new_outpoints) in addr_map {
            let mut set = load_address_set_write(&tx, &self.addresses_partition, &addr)?;
            set.extend(new_outpoints);
            store_address_set_write(&mut tx, &self.addresses_partition, &addr, &set)?;
        }

        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }

    /// Batch-removes multiple UTXOs, removing them from both `utxo_partition` and the address sets.
    /// If any outpoint doesn't exist, we return `NotFound` and abort the entire transaction.
    async fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>
    ) -> Result<(), Self::StorageError> {
        let mut tx = self.keyspace.write_tx();

        // For each address, which outpoints must we remove from its set?
        let mut removal_map: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();

        // 1) Remove from main partition after verifying existence
        for op in &outpoints {
            let raw_opt = tx.get(&self.utxo_partition, Slice::from(&encode_utxo_key(op)))
                .map_err(StryiStorageError::FjallError)?;
            let raw = raw_opt.ok_or_else(|| {
                StryiStorageError::NotFound(format!("UTXO not found for outpoint: {:?}", op))
            })?;

            // decode the UTXO to see which address it belongs to
            let (utxo, _) = bincode::serde::decode_from_slice::<UTXO, _>(&raw, standard())
                .map_err(StryiStorageError::DeserializationError)?;

            // remove it
            tx.remove(&self.utxo_partition, Slice::from(&encode_utxo_key(op)));
            removal_map
                .entry(utxo.owner)
                .or_insert_with(HashSet::new)
                .insert(op.clone());
        }

        // 2) For each address, remove these outpoints from the index
        for (addr, ops) in removal_map {
            let mut set = load_address_set_write(&tx, &self.addresses_partition, &addr)?;
            for o in ops {
                set.remove(&o);
            }
            store_address_set_write(&mut tx, &self.addresses_partition, &addr, &set)?;
        }

        // 3) commit
        tx.commit().map_err(StryiStorageError::FjallError)?;
        Ok(())
    }
}
