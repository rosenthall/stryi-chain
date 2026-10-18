use crate::{ReadSource, StorageView};
use std::collections::{HashMap, HashSet};
use stryi_core::address::AccountAddress;
use stryi_core::storage::{StorageError, UtxoStorage};
use stryi_core::transactions::{OutPoint, UTXO};

/// Encode `(txid, vout)` into the 36-byte UTXO key.
pub fn encode_utxo_key(outpoint: &OutPoint) -> [u8; 36] {
    let mut key = [0; 36];
    key[..32].copy_from_slice(&outpoint.txid.data);
    key[32..].copy_from_slice(&outpoint.vout.to_be_bytes());

    key
}

impl<R: ReadSource> UtxoStorage for StorageView<'_, R> {
    fn get_utxo(&self, outpoint: OutPoint) -> Result<Option<UTXO>, StorageError> {
        let value: Option<UTXO> = self.get("utxo", &encode_utxo_key(&outpoint))?;
        if let Some(utxo) = value
            && (utxo.txid != outpoint.txid || utxo.vout != outpoint.vout)
        {
            return Err(self.report_corruption(format!("UTXO value does not match key {outpoint}")));
        }

        Ok(value)
    }

    fn get_utxos_for_address(
        &self,
        address: AccountAddress,
    ) -> Result<HashMap<OutPoint, UTXO>, StorageError> {
        let outpoints: HashSet<OutPoint> =
            self.get("addresses", &address.data)?.unwrap_or_default();
        let mut utxos = HashMap::with_capacity(outpoints.len());

        for outpoint in outpoints {
            let utxo = self.get_utxo(outpoint)?.ok_or_else(|| {
                self.report_corruption(format!(
                    "Address {address} references missing UTXO {outpoint}"
                ))
            })?;
            if utxo.owner != address {
                return Err(self.report_corruption(format!(
                    "Address {address} references UTXO owned by {}",
                    utxo.owner
                )));
            }

            utxos.insert(outpoint, utxo);
        }

        Ok(utxos)
    }
}
