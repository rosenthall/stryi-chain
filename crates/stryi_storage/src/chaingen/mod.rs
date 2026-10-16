use crate::{StryiRead, StryiStorageError};
use rocksdb::IteratorMode;
use std::collections::HashMap;
use stryi_core::address::AccountAddress;
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::{OutPoint, UTXO};

/// Maps account addresses to their unspent transaction outputs.
/// Structure: `Address -> (OutPoint -> UTXO)`
pub type AccountUtxoMap = HashMap<AccountAddress, HashMap<OutPoint, UTXO>>;

/// Chaingen scans over one snapshot, keeping address membership and UTXOs consistent.
pub trait StryiStorageChaingenExt {
    /// Retrieves all existing UTXOs from storage.
    ///
    /// Returns accounts mapped to their outputs: `Address -> (OutPoint -> UTXO)`
    fn get_all_utxos(&self) -> Result<AccountUtxoMap, StryiStorageError>;
}

impl StryiStorageChaingenExt for StryiRead<'_> {
    fn get_all_utxos(&self) -> Result<AccountUtxoMap, StryiStorageError> {
        let mut accounts = HashMap::new();

        for entry in self
            .reader
            .iterator_cf(self.inner.table("addresses"), IteratorMode::Start)
        {
            let (key, _) = entry.map_err(|error| {
                self.inner.mark_failed();
                crate::error::backend_error("iterate addresses", error)
            })?;

            let bytes: [u8; 20] = key
                .as_ref()
                .try_into()
                .map_err(|_| self.report_corruption("Invalid address key length"))?;
            let address = AccountAddress::try_from(bytes.as_slice())
                .map_err(|error| self.report_corruption(format!("Invalid address key: {error}")))?;

            accounts.insert(address, self.get_utxos_for_address(address)?);
        }

        Ok(accounts)
    }
}
