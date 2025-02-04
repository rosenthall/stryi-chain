use std::collections::{HashMap, HashSet};
use stryi_core::address::AccountAddress;
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::{OutPoint, UTXO};
use crate::{StryiStorage, StryiStorageError};

impl UtxoStorage for StryiStorage {
    type StorageError = StryiStorageError;

    async fn get_utxos_for_address(&self, address: &AccountAddress) -> Result<Vec<(OutPoint, UTXO)>, Self::StorageError> {
        todo!("Implement")
    }

    async fn get_utxo(&self, outpoint: &OutPoint) -> Result<UTXO, Self::StorageError> {
        todo!("Implement")
    }

    async fn get_utxos(&self, outpoints: &HashSet<OutPoint>) -> Result<HashMap<OutPoint, UTXO>, Self::StorageError> {
        todo!("Implement")
    }

    async fn put_utxo(&mut self, outpoint: &OutPoint, utxo: UTXO) -> Result<(), Self::StorageError> {
        todo!("Implement")
    }

    async fn remove_utxo(&mut self, outpoint: &OutPoint) -> Result<(), Self::StorageError> {
        todo!("Implement")
    }

    async fn batch_put_utxos(&mut self, utxos: Vec<(OutPoint, UTXO)>) -> Result<(), Self::StorageError> {
        todo!("Implement")
    }

    async fn batch_remove_utxos(&mut self, outpoints: Vec<OutPoint>) -> Result<(), Self::StorageError> {
        todo!("Implement")
    }
}