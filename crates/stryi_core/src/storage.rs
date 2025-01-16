use crate::transactions::{OutPoint, UTXO};

/// Trait representing storage backend for UTXOs.
pub trait UtxoStorage {
    /// Returns `Some(UTXO)` if the specified outpoint is unspent, or `None` if not found/spent.
    fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO>;

    /// Inserts or updates a UTXO in storage.
    fn put_utxo(&mut self, outpoint: &OutPoint, utxo: UTXO);

    /// Removes a UTXO from storage (marks it as spent).
    fn remove_utxo(&mut self, outpoint: &OutPoint);
}
