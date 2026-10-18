//! RocksDB storage for blocks, UTXOs, canonical indexes, and rollback data.
//! See `TABLES.md` for the persistent schema.

#![allow(incomplete_features)]
// This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199
#![feature(generic_const_exprs)]

mod blocks;
mod error;
mod meta;
mod open;
mod stats;
mod undo;
mod utxo;
mod write;

/// Utilities specific to `chaingen` feature.
/// It provides APIs used exclusively by the chain generation tooling (`stryi_chaingen`).
#[cfg(feature = "chaingen")]
pub mod chaingen;
#[cfg(test)]
mod tests;

pub use error::StryiStorageError;
pub use meta::{STORAGE_SCHEMA_VERSION, StorageStatus};
pub use stryi_core::storage::{
    BlockIndexData, ChainMetadata, TransactionIndexData, validate_genesis,
};

use indexmap::IndexMap;
use rocksdb::{
    ColumnFamily, SingleThreaded, SnapshotWithThreadMode, Transaction, TransactionDB,
    TransactionOptions, WriteOptions,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, GenesisState};
use stryi_core::storage::{
    ChainTransition, ChainTransitionResult, FullNodeStorage, StorageError, StorageReader,
    apply_transition,
};
use stryi_core::transactions::{OutPoint, UTXO};

pub(crate) type Database = TransactionDB<SingleThreaded>;
const TABLES: [&str; 9] = [
    "blocks",
    "heights",
    "utxo",
    "addresses",
    "stats",
    "undo",
    "block_indexes",
    "transaction_indexes",
    "metadata",
];
const STATE_KEY: [u8; 32] = [0; 32];
const METADATA_KEY: &[u8] = b"chain";

/// Shared storage handle over the RocksDB database and its fixed column families.
/// Cloning a handle shares the database and writer lock; it does not capture a snapshot.
#[derive(Clone)]
pub struct StryiStorage {
    inner: Arc<StorageInner>,
}

struct StorageInner {
    db: Database,
    // One canonical transition at a time, including its reads and pending writes.
    writer: Mutex<()>,
    // A backend/commit failure or panic permanently stops this handle.
    failed: AtomicBool,
    failure_handler: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

/// Wrapper around a RocksDB snapshot or active transaction.
pub struct StorageView<'a, R> {
    inner: &'a StorageInner,
    reader: R,
}

/// Consistent read snapshot across blocks, chain state, UTXOs and their indexes.
pub type StryiRead<'a> = StorageView<'a, SnapshotWithThreadMode<'a, Database>>;
type WriteView<'a> = StorageView<'a, Transaction<'a, Database>>;

trait ReadSource {
    fn get(&self, table: &ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, rocksdb::Error>;
}

impl ReadSource for SnapshotWithThreadMode<'_, Database> {
    fn get(&self, table: &ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, rocksdb::Error> {
        self.get_cf(table, key)
    }
}

impl ReadSource for Transaction<'_, Database> {
    fn get(&self, table: &ColumnFamily, key: &[u8]) -> Result<Option<Vec<u8>>, rocksdb::Error> {
        // TransactionOptions::set_snapshot does not pin ordinary transaction reads.
        self.snapshot().get_cf(table, key)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
// This struct stores the data needed to create a custom genesis block: balances for each address, plus block header fields.
pub struct GenesisInitConfig {
    pub wanted_balances: IndexMap<AccountAddress, u64>,
    pub genesis_state: GenesisState,
    pub version: u16,
}

#[cfg(test)]
impl GenesisInitConfig {
    /// Build a minimal genesis config for tests.
    /// NOTE: Genesis, by convention, must have at least one allocation
    pub fn new_test() -> Self {
        let mut wanted_balances = IndexMap::new();

        wanted_balances.insert(AccountAddress::new(&[0u8; 20]), 1_000_000);

        Self {
            wanted_balances,
            genesis_state: GenesisState::default(),
            version: 0,
        }
    }
}

/// Extract all block outputs as `(OutPoint, UTXO)` pairs.
pub fn extract_utxos_from_block(block: &Block) -> Vec<(OutPoint, UTXO)> {
    block
        .data
        .transactions
        .iter()
        .flat_map(|tx| {
            tx.data
                .outputs
                .iter()
                .enumerate()
                .map(move |(vout, output)| {
                    let outpoint = OutPoint {
                        txid: tx.data.hash(),
                        vout: vout as u32,
                    };

                    let utxo = UTXO {
                        txid: tx.data.hash(),
                        vout: vout as u32,
                        value: output.value,
                        owner: output.recipient,
                    };

                    (outpoint, utxo)
                })
        })
        .collect()
}

impl StryiStorage {
    /// Whether a fatal failure has permanently stopped this storage handle.
    pub fn is_failed(&self) -> bool {
        self.inner.failed.load(Ordering::Acquire) || self.inner.writer.is_poisoned()
    }

    /// Permanently marks storage as failed on critical errors or panics.
    pub fn mark_failed(&self) {
        self.inner.mark_failed();
    }

    /// Registers a callback invoked on fatal storage failure (fires immediately if already failed).
    pub fn set_failure_handler(&self, handler: Arc<dyn Fn() + Send + Sync>) {
        let handler = {
            let mut registered = self
                .inner
                .failure_handler
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());

            registered.get_or_insert(handler).clone()
        };

        if self.is_failed() {
            handler();
        }
    }

    /// Runs `callback` inside a write transaction under the writer lock and commits on `Ok`.
    /// On callback error the transaction is rolled back; on commit error or panic the handle is marked failed.
    fn update<T>(
        &self,
        callback: impl FnOnce(&mut WriteView<'_>) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        if self.is_failed() {
            return Err(StorageError::Unhealthy);
        }

        let _writer = self.inner.writer.lock().map_err(|_| {
            self.mark_failed();
            StorageError::Unhealthy
        })?;

        if self.is_failed() {
            return Err(StorageError::Unhealthy);
        }

        // Force WAL fsync on commit
        let mut writes = WriteOptions::default();
        writes.set_sync(true);
        writes.disable_wal(false);

        let mut options = TransactionOptions::default();
        options.set_snapshot(true);

        let mut view = StorageView {
            inner: &self.inner,
            reader: self.inner.db.transaction_opt(&writes, &options),
        };

        let result = match catch_unwind(AssertUnwindSafe(|| callback(&mut view))) {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                if error.is_fatal() {
                    self.mark_failed();
                }

                return Err(error);
            }
            Err(panic) => {
                self.mark_failed();
                resume_unwind(panic);
            }
        };

        if self.is_failed() {
            return Err(StorageError::Unhealthy);
        }

        if let Err(error) = view.reader.commit() {
            self.mark_failed();
            return Err(StorageError::Commit(Arc::new(error)));
        }

        Ok(result)
    }
}

impl StorageReader for StryiStorage {
    type Read<'a> = StryiRead<'a>;

    fn read(&self) -> Result<Self::Read<'_>, StorageError> {
        if self.is_failed() {
            return Err(StorageError::Unhealthy);
        }

        Ok(StorageView {
            inner: &self.inner,
            reader: self.inner.db.snapshot(),
        })
    }
}

impl FullNodeStorage for StryiStorage {
    fn apply_transition(
        &self,
        transition: ChainTransition<'_>,
    ) -> Result<ChainTransitionResult, StorageError> {
        self.update(|view| {
            if let ChainTransition::Genesis { metadata, .. } = &transition
                && metadata.schema_version != STORAGE_SCHEMA_VERSION
            {
                return Err(StorageError::UnsupportedSchema(metadata.schema_version));
            }

            apply_transition(view, transition)
        })
    }
}

impl StorageInner {
    fn mark_failed(&self) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            let handler = self
                .failure_handler
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();

            if let Some(handler) = handler {
                handler();
            }
        }
    }

    fn table(&self, name: &str) -> &ColumnFamily {
        self.db
            .cf_handle(name)
            .expect("column families are fixed and verified on open")
    }
}

impl<R> StorageView<'_, R> {
    fn raw_get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>
    where
        R: ReadSource,
    {
        if self.inner.failed.load(Ordering::Acquire) || self.inner.writer.is_poisoned() {
            return Err(StorageError::Unhealthy);
        }

        self.reader
            .get(self.inner.table(table), key)
            .map_err(|error| {
                self.inner.mark_failed();
                error::backend_error(format!("read {table}"), error)
            })
    }

    fn get<T: DeserializeOwned>(&self, table: &str, key: &[u8]) -> Result<Option<T>, StorageError>
    where
        R: ReadSource,
    {
        self.raw_get(table, key)?
            .map(|bytes| self.decode(table, &bytes))
            .transpose()
    }

    fn decode<T: DeserializeOwned>(&self, table: &str, bytes: &[u8]) -> Result<T, StorageError> {
        postcard::from_bytes(bytes)
            .map_err(|error| self.report_corruption(format!("Invalid {table} value: {error}")))
    }

    /// Permanently fail the shared handle and notify its failure handler.
    fn report_corruption(&self, details: impl Into<String>) -> StorageError {
        self.inner.mark_failed();
        StorageError::Corruption(details.into())
    }
}

impl WriteView<'_> {
    fn put<T: Serialize>(&self, table: &str, key: &[u8], value: &T) -> Result<(), StorageError> {
        let bytes = postcard::to_stdvec(value)
            .map_err(|error| error::backend_error(format!("serialize {table}"), error))?;

        self.reader
            .put_cf(self.inner.table(table), key, bytes)
            .map_err(|error| error::backend_error(format!("write {table}"), error))
    }

    fn remove(&self, table: &str, key: &[u8]) -> Result<(), StorageError> {
        self.reader
            .delete_cf(self.inner.table(table), key)
            .map_err(|error| error::backend_error(format!("remove {table}"), error))
    }
}
