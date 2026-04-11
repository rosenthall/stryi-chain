use crate::StryiStorage;
use crate::error::StryiStorageError;
use bincode::config::standard;
use fjall::{ReadTransaction, Slice, WriteTransaction};
use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};

use stryi_core::address::AccountAddress;
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::{OutPoint, UTXO};

/// Encode `(txid, vout)` into the 36-byte UTXO key.
pub fn encode_utxo_key(outpoint: &OutPoint) -> [u8; 36] {
    let mut key = [0u8; 36];
    key[..32].copy_from_slice(&outpoint.txid.data);
    key[32..36].copy_from_slice(&outpoint.vout.to_be_bytes());
    key
}

fn decode_outpoints_set(bytes: &[u8]) -> Result<HashSet<OutPoint>, StryiStorageError> {
    let (set, _) = bincode::serde::decode_from_slice::<HashSet<OutPoint>, _>(bytes, standard())
        .map_err(StryiStorageError::DeserializationError)?;
    Ok(set)
}

fn encode_outpoints_set(set: &HashSet<OutPoint>) -> Result<Vec<u8>, StryiStorageError> {
    bincode::serde::encode_to_vec(set, standard()).map_err(StryiStorageError::SerializationError)
}

/// Load an address outpoint set, or return an empty set.
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

/// Load an address outpoint set inside a write transaction.
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

/// Store the updated outpoint set for an address.
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
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        let outcome: Result<(), Self::StorageError> = (|| {
            let mut tx = self.keyspace.write_tx();
            let up = self.utxo_partition.clone();
            let ap = self.addresses_partition.clone();

            let mut addr_map: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();
            for (op, u) in utxos {
                let key = encode_utxo_key(&op);
                let bytes = bincode::serde::encode_to_vec(u, standard())
                    .map_err(StryiStorageError::SerializationError)?;
                tx.insert(&up, Slice::from(&key), Slice::from(bytes));

                addr_map.entry(u.owner).or_default().insert(op);
            }

            for (addr, new_set) in addr_map {
                let mut existing = load_address_set_write(&tx, &ap, &addr)?;
                existing.extend(new_set);
                store_address_set_write(&mut tx, &ap, &addr, &existing)?;
            }

            tx.commit().map_err(StryiStorageError::FjallError)?;
            Ok(())
        })();

        Box::pin(async move { outcome })
    }

    fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        let result: Result<(), Self::StorageError> = (|| {
            let mut tx = self.keyspace.write_tx();
            let up = self.utxo_partition.clone();
            let ap = self.addresses_partition.clone();

            let mut removal_map: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();

            for op in outpoints {
                let key = encode_utxo_key(&op);
                let raw_opt = tx
                    .get(&up, Slice::from(&key))
                    .map_err(StryiStorageError::FjallError)?;
                let raw = raw_opt.ok_or_else(|| {
                    StryiStorageError::NotFound(format!("UTXO not found: {:?}", op))
                })?;
                let (utxo, _) = bincode::serde::decode_from_slice::<UTXO, _>(&raw, standard())
                    .map_err(StryiStorageError::DeserializationError)?;

                tx.remove(&up, Slice::from(&key));
                removal_map.entry(utxo.owner).or_default().insert(op);
            }

            for (addr, ops) in removal_map {
                let mut existing = load_address_set_write(&tx, &ap, &addr)?;
                for op in ops {
                    existing.remove(&op);
                }
                store_address_set_write(&mut tx, &ap, &addr, &existing)?;
            }

            tx.commit().map_err(StryiStorageError::FjallError)?;
            Ok(())
        })();

        Box::pin(async move { result })
    }

    fn batch_get_utxos<I>(
        &self,
        outpoints: I,
    ) -> BoxFuture<'_, Result<HashMap<OutPoint, UTXO>, Self::StorageError>>
    where
        I: IntoIterator<Item = OutPoint> + Send,
        I::IntoIter: Send,
    {
        let read_tx = self.keyspace.read_tx();
        let utxo_partition = self.utxo_partition.clone();
        let ops: Vec<OutPoint> = outpoints.into_iter().collect();

        Box::pin(async move {
            let mut result = HashMap::with_capacity(ops.len());

            for op in ops.into_iter() {
                let key = encode_utxo_key(&op);
                let raw_opt = read_tx
                    .get(&utxo_partition, Slice::from(&key[..]))
                    .map_err(StryiStorageError::FjallError)?;
                let raw = raw_opt.ok_or_else(|| {
                    StryiStorageError::NotFound(format!("UTXO not found for outpoint {:?}", op))
                })?;

                let (utxo, _) = bincode::serde::decode_from_slice::<UTXO, _>(&raw, standard())
                    .map_err(StryiStorageError::DeserializationError)?;

                result.insert(op, utxo);
            }

            Ok(result)
        })
    }

    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> BoxFuture<'_, Result<HashMap<OutPoint, UTXO>, Self::StorageError>> {
        let read_tx = self.keyspace.read_tx();

        Box::pin(async move {
            let outpoints_set =
                load_address_set_read(&read_tx, &self.addresses_partition, &address)?;

            let utxos = self.batch_get_utxos(outpoints_set).await?;

            Ok(utxos)
        })
    }
}
