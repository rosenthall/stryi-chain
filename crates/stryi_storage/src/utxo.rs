use crate::StryiStorage;
use crate::error::StryiStorageError;
use fjall::{Readable, Slice, Snapshot};
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
    let set = postcard::from_bytes::<HashSet<OutPoint>>(bytes)
        .map_err(StryiStorageError::DeserializationError)?;
    Ok(set)
}

fn encode_outpoints_set(set: &HashSet<OutPoint>) -> Result<Vec<u8>, StryiStorageError> {
    postcard::to_stdvec(set).map_err(StryiStorageError::SerializationError)
}

/// Load an address outpoint set from a snapshot.
fn load_address_set(
    snapshot: &Snapshot,
    partition: &fjall::Keyspace,
    address: &AccountAddress,
) -> Result<HashSet<OutPoint>, StryiStorageError> {
    let data_opt = snapshot
        .get(partition, &address.data[..])
        .map_err(StryiStorageError::FjallError)?;

    match data_opt {
        Some(value) => decode_outpoints_set(&value),
        None => Ok(HashSet::new()),
    }
}

impl UtxoStorage for StryiStorage {
    type StorageError = StryiStorageError;

    fn batch_put_utxos(
        &mut self,
        utxos: Vec<(OutPoint, UTXO)>,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        let snapshot = self.db.snapshot();
        let up = self.utxo_partition.clone();
        let ap = self.addresses_partition.clone();

        let outcome: Result<(), Self::StorageError> = (|| {
            let mut batch = self.db.batch();
            let mut addr_map: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();

            for (op, u) in utxos {
                let key = encode_utxo_key(&op);
                let bytes =
                    postcard::to_stdvec(&u).map_err(StryiStorageError::SerializationError)?;
                batch.insert(&up, Slice::from(&key), Slice::from(bytes));

                addr_map.entry(u.owner).or_default().insert(op);
            }

            for (addr, new_set) in addr_map {
                let mut existing = load_address_set(&snapshot, &ap, &addr)?;
                existing.extend(new_set);
                let encoded = encode_outpoints_set(&existing)?;
                batch.insert(&ap, Slice::from(&addr.data[..]), Slice::from(encoded));
            }

            batch.commit().map_err(StryiStorageError::FjallError)?;
            Ok(())
        })();

        Box::pin(async move { outcome })
    }

    fn batch_remove_utxos(
        &mut self,
        outpoints: Vec<OutPoint>,
    ) -> BoxFuture<'_, Result<(), Self::StorageError>> {
        let snapshot = self.db.snapshot();
        let up = self.utxo_partition.clone();
        let ap = self.addresses_partition.clone();

        let result: Result<(), Self::StorageError> = (|| {
            let mut batch = self.db.batch();
            let mut removal_map: HashMap<AccountAddress, HashSet<OutPoint>> = HashMap::new();

            for op in outpoints {
                let key = encode_utxo_key(&op);
                let raw_opt = snapshot
                    .get(&up, key)
                    .map_err(StryiStorageError::FjallError)?;
                let raw = raw_opt.ok_or_else(|| {
                    StryiStorageError::NotFound(format!("UTXO not found: {:?}", op))
                })?;
                let utxo = postcard::from_bytes::<UTXO>(&raw)
                    .map_err(StryiStorageError::DeserializationError)?;

                batch.remove(&up, Slice::from(&key));
                removal_map.entry(utxo.owner).or_default().insert(op);
            }

            for (addr, ops) in removal_map {
                let mut existing = load_address_set(&snapshot, &ap, &addr)?;
                for op in ops {
                    existing.remove(&op);
                }
                let encoded = encode_outpoints_set(&existing)?;
                batch.insert(&ap, Slice::from(&addr.data[..]), Slice::from(encoded));
            }

            batch.commit().map_err(StryiStorageError::FjallError)?;
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
        let snapshot = self.db.snapshot();
        let utxo_partition = self.utxo_partition.clone();
        let ops: Vec<OutPoint> = outpoints.into_iter().collect();

        Box::pin(async move {
            let mut result = HashMap::with_capacity(ops.len());

            for op in ops.into_iter() {
                let key = encode_utxo_key(&op);
                let raw_opt = snapshot
                    .get(&utxo_partition, &key[..])
                    .map_err(StryiStorageError::FjallError)?;
                let raw = raw_opt.ok_or_else(|| {
                    StryiStorageError::NotFound(format!("UTXO not found for outpoint {:?}", op))
                })?;

                let utxo = postcard::from_bytes::<UTXO>(&raw)
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
        let snapshot = self.db.snapshot();

        Box::pin(async move {
            let outpoints_set = load_address_set(&snapshot, &self.addresses_partition, &address)?;

            let utxos = self.batch_get_utxos(outpoints_set).await?;

            Ok(utxos)
        })
    }
}
