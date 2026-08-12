use crate::StryiStorage;
use crate::error::StryiStorageError;
use crate::utxo::encode_utxo_key;
use fjall::Readable;
use serde::de::Error;
use futures_core::future::BoxFuture;
use std::collections::{HashMap, HashSet};
use stryi_core::address::AccountAddress;
use stryi_core::transactions::{OutPoint, UTXO};

/// Maps account addresses to their unspent transaction outputs.
/// Structure: `Address -> (OutPoint -> UTXO)`
pub type AccountUtxoMap = HashMap<AccountAddress, HashMap<OutPoint, UTXO>>;

/// Extension trait for chain generation operations.
pub trait StryiStorageChaingenExt: Send + Sync {
    /// Retrieves all existing UTXOs from storage.
    ///
    /// Returns accounts mapped to their outputs: `Address -> (OutPoint -> UTXO)`
    fn get_all_utxos(&self) -> BoxFuture<'_, Result<AccountUtxoMap, StryiStorageError>>;
}

impl StryiStorageChaingenExt for StryiStorage {
    fn get_all_utxos(&self) -> BoxFuture<'_, Result<AccountUtxoMap, StryiStorageError>> {
        let snapshot = self.db.snapshot();
        let addresses_partition = self.addresses_partition.clone();
        let utxo_partition = self.utxo_partition.clone();

        Box::pin(async move {
            let mut result = AccountUtxoMap::new();
            let iter = snapshot.prefix(&addresses_partition, []);

            for item in iter {
                let (key_slice, value_slice) = item.into_inner()
                    .map_err(StryiStorageError::FjallError)?;

                // Decode address (20 bytes)
                if key_slice.len() != 20 {
                    return Err(StryiStorageError::DeserializationError(
                        Error::custom("Invalid address key length"),
                    ));
                }

                let mut address_data = [0u8; 20];
                address_data.copy_from_slice(&key_slice);
                let address = AccountAddress::try_from(&address_data[..]).map_err(|_| {
                    StryiStorageError::DeserializationError(Error::custom(
                        "Invalid address data",
                    ))
                })?;

                // Decode outpoints set
                let outpoints_set = postcard::from_bytes::<HashSet<OutPoint>>(&value_slice)
                    .map_err(StryiStorageError::DeserializationError)?;

                // Fetch all UTXOs for this address
                let mut utxos = HashMap::new();
                for outpoint in outpoints_set {
                    let key = encode_utxo_key(&outpoint);
                    let encoded_utxo = snapshot.get(&utxo_partition, key)?.ok_or_else(|| {
                        StryiStorageError::NotFound(format!(
                            "UTXO not found for outpoint {}",
                            outpoint
                        ))
                    })?;

                    let utxo =
                        postcard::from_bytes::<UTXO>(&encoded_utxo)
                            .map_err(StryiStorageError::DeserializationError)?;

                    utxos.insert(outpoint, utxo);
                }

                result.insert(address, utxos);
            }

            Ok(result)
        })
    }
}
