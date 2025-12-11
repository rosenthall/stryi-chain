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
//!    This is an index from address -> the set of outpoints that belong to that address. We keep
//!    this index in sync whenever we add or remove a UTXO in the main partition, so that
//!    `get_utxos_for_address` does not require scanning all UTXOs.

use crate::StryiStorage;
use crate::error::StryiStorageError;
use bincode::config::standard;
use fjall::{ReadTransaction, Slice, WriteTransaction};
use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};

use stryi_core::address::AccountAddress;
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::{OutPoint, UTXO};

/// Encodes an outpoint into a 36-byte key:
/// - bytes [0..32]: `txid.data`
/// - bytes [32..36]: `vout` as a big-endian 32-bit integer.
///   We use 4 bytes to handle large numbers of outputs if needed.
pub fn encode_utxo_key(outpoint: &OutPoint) -> [u8; 36] {
    let mut key = [0u8; 36];
    key[..32].copy_from_slice(&outpoint.txid.data);
    key[32..36].copy_from_slice(&outpoint.vout.to_be_bytes());
    key
}

/// Decodes a bincode `HashSet<OutPoint>` from a slice of bytes, returning an empty set if `None`.
pub(crate) fn decode_outpoints_set(bytes: &[u8]) -> Result<HashSet<OutPoint>, StryiStorageError> {
    let (set, _) = bincode::serde::decode_from_slice::<HashSet<OutPoint>, _>(bytes, standard())
        .map_err(StryiStorageError::DeserializationError)?;
    Ok(set)
}

/// Encodes a `HashSet<OutPoint>` into bytes using bincode.
fn encode_outpoints_set(ops: &HashSet<OutPoint>) -> Result<Vec<u8>, StryiStorageError> {
    bincode::serde::encode_to_vec(ops, standard()).map_err(StryiStorageError::SerializationError)
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
    address: &AccountAddress,
) -> Result<HashSet<OutPoint>, StryiStorageError> {
    let data_opt = read_tx
        .get(partition, Slice::from(&address.data[..]))
        .map_err(StryiStorageError::FjallError)?;

    match data_opt {
        Some(slice) => decode_outpoints_set(&slice),
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
    address: &AccountAddress,
) -> Result<HashSet<OutPoint>, StryiStorageError> {
    let data_opt = write_tx
        .get(partition, Slice::from(&address.data[..]))
        .map_err(StryiStorageError::FjallError)?;

    match data_opt {
        Some(slice) => decode_outpoints_set(&slice),
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
    write_tx.insert(
        partition,
        Slice::from(&address.data[..]),
        Slice::from(encoded),
    );
    Ok(())
}

impl UtxoStorage for StryiStorage {
    type StorageError = StryiStorageError;

    fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>,
    ) -> BoxFuture<Result<(), Self::StorageError>> {
        // Do all the writes, map‐building, and commit in one synchronous block.
        let outcome: Result<(), Self::StorageError> = (|| {
            // Initialize write transaction and clone partitions we need.
            let mut tx = self.keyspace.write_tx();
            let up = self.utxo_partition.clone();
            let ap = self.addresses_partition.clone();

            // collect new outpoints per address
            let mut addr_map: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();
            for (op, u) in utxos {
                // insert into the UTXO partition
                let key = encode_utxo_key(&op);
                let bytes = bincode::serde::encode_to_vec(u, standard())
                    .map_err(StryiStorageError::SerializationError)?;
                tx.insert(&up, Slice::from(&key), Slice::from(bytes));

                // record for the address index
                addr_map.entry(u.owner).or_default().insert(op);
            }

            // update each address’s set
            for (addr, new_set) in addr_map {
                let mut existing = load_address_set_write(&tx, &ap, &addr)?;
                existing.extend(new_set);
                store_address_set_write(&mut tx, &ap, &addr, &existing)?;
            }

            // commit once
            tx.commit().map_err(StryiStorageError::FjallError)?;
            Ok(())
        })();

        // Return a future that’s immediately ready with that result.
        Box::pin(async move { outcome })
    }

    fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>,
    ) -> BoxFuture<Result<(), Self::StorageError>> {
        // Perform all removal logic synchronously
        let result: Result<(), Self::StorageError> = (|| {
            // Initialize write transaction and clone partitions we need.
            let mut tx = self.keyspace.write_tx();
            let up = self.utxo_partition.clone();
            let ap = self.addresses_partition.clone();

            // map addresses to the set of outpoints we need to remove
            let mut removal_map: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();

            for op in outpoints {
                // Read the UTXO to find its owner
                let key = encode_utxo_key(&op);
                let raw_opt = tx
                    .get(&up, Slice::from(&key))
                    .map_err(StryiStorageError::FjallError)?;
                let raw = raw_opt.ok_or_else(|| {
                    StryiStorageError::NotFound(format!("UTXO not found: {:?}", op))
                })?;
                let (utxo, _) = bincode::serde::decode_from_slice::<UTXO, _>(&raw, standard())
                    .map_err(StryiStorageError::DeserializationError)?;

                // Remove from the main partition
                tx.remove(&up, Slice::from(&key));
                // Record for updating the address index
                removal_map.entry(utxo.owner).or_default().insert(op);
            }

            // Update each address’s outpoint set
            for (addr, ops) in removal_map {
                let mut existing = load_address_set_write(&tx, &ap, &addr)?;
                for op in ops {
                    existing.remove(&op);
                }
                store_address_set_write(&mut tx, &ap, &addr, &existing)?;
            }

            // Commit once
            tx.commit().map_err(StryiStorageError::FjallError)?;
            Ok(())
        })();

        // Return a “ready” future that just yields `result`
        Box::pin(async move { result })
    }

    fn batch_get_utxos<I>(
        &self,
        outpoints: I,
    ) -> BoxFuture<Result<HashMap<OutPoint, UTXO>, Self::StorageError>>
    where
        I: IntoIterator<Item = OutPoint> + Send,
        I::IntoIter: Send,
    {
        // start a single read‐only transaction
        let read_tx = self.keyspace.read_tx();
        // clone the partition handle for the async move
        let utxo_partition = self.utxo_partition.clone();
        // collect so we know how many and can iterate inside the future
        let ops: Vec<OutPoint> = outpoints.into_iter().collect();

        Box::pin(async move {
            let mut result = HashMap::with_capacity(ops.len());

            for op in ops.into_iter() {
                // build the 36‐byte key
                let key = encode_utxo_key(&op);
                // fetch raw bytes
                let raw_opt = read_tx
                    .get(&utxo_partition, Slice::from(&key[..]))
                    .map_err(StryiStorageError::FjallError)?;
                let raw = raw_opt.ok_or_else(|| {
                    StryiStorageError::NotFound(format!("UTXO not found for outpoint {:?}", op))
                })?;

                // decode the UTXO
                let (utxo, _) = bincode::serde::decode_from_slice::<UTXO, _>(&raw, standard())
                    .map_err(StryiStorageError::DeserializationError)?;
                // insert into our result map
                result.insert(op, utxo);
            }

            Ok(result)
        })
    }

    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> BoxFuture<Result<HashMap<OutPoint, UTXO>, Self::StorageError>> {
        // Setup read transaction
        let read_tx = self.keyspace.read_tx();

        Box::pin(async move {
            // Load all the outputs from `address` partition
            let outpoints_set =
                load_address_set_read(&read_tx, &self.addresses_partition, &address)?;

            // batch read all utxos
            let utxos = self.batch_get_utxos(outpoints_set).await?;

            Ok(utxos)
        })
    }
}
