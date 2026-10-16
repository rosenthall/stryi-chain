//! RocksDB storage for blocks, UTXOs, canonical indexes, and rollback data.
//! See `TABLES.md` for the persistent schema.

#![allow(incomplete_features)]
// This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199
#![feature(generic_const_exprs)]

mod blocks;
mod error;
mod meta;
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
    ColumnFamily, ColumnFamilyDescriptor, DBCompressionType, IteratorMode, Options, SingleThreaded,
    SnapshotWithThreadMode, Transaction, TransactionDB, TransactionDBOptions, TransactionOptions,
    WriteOptions,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{SystemTime, UNIX_EPOCH};
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash, GenesisState};
use stryi_core::difficulty::ChainWork;
use stryi_core::storage::{
    BlockStorage, ChainState, ChainTransition, ChainTransitionResult, FullNodeStorage,
    StorageError, StorageReader, StorageStats, apply_transition,
};
use stryi_core::transactions::{OutPoint, UTXO};
use tracing::info;

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
    /// Opens or creates RocksDB storage at `path` (rejecting old Fjall or non-empty foreign dirs).
    /// If `genesis_config` is provided, initializes genesis atomically or checks that it matches stored one.
    pub fn initialize_in_path(
        path: PathBuf,
        genesis_config: Option<GenesisInitConfig>,
    ) -> Result<Self, StorageError> {
        let genesis = genesis_config.map(|config| {
            Block::new_genesis(
                config.version,
                HashMap::from_iter(config.wanted_balances),
                config.genesis_state,
            )
        });

        if let Some(block) = &genesis {
            validate_genesis(block)?;
        }

        let storage = Self::open_database(&path)?;
        storage.verify_initialized_state()?;

        if let Some(block) = genesis {
            let metadata = storage.metadata_for_genesis()?;

            storage.apply_transition(ChainTransition::Genesis {
                block: &block,
                metadata: &metadata,
            })?;
        }

        info!("Opened RocksDB storage at {}", path.display());

        Ok(storage)
    }

    fn open_database(path: &Path) -> Result<Self, StorageError> {
        let existing = check_supported_datadir(path)?;

        let mut options = Options::default();
        options.create_if_missing(!existing);
        options.create_missing_column_families(!existing);
        options.set_compression_type(DBCompressionType::Lz4);

        let tables = TABLES.map(|name| {
            let mut options = Options::default();
            options.set_compression_type(DBCompressionType::Lz4);
            ColumnFamilyDescriptor::new(name, options)
        });

        let db =
            Database::open_cf_descriptors(&options, &TransactionDBOptions::default(), path, tables)
                .map_err(|error| error::backend_error("open database", error))?;

        Ok(Self {
            inner: Arc::new(StorageInner {
                db,
                writer: Mutex::new(()),
                failed: AtomicBool::new(false),
                failure_handler: Mutex::new(None),
            }),
        })
    }

    fn metadata_for_genesis(&self) -> Result<ChainMetadata, StorageError> {
        if let Some(metadata) = self.read()?.metadata()? {
            return Ok(metadata);
        }

        let created_unix_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error::backend_error("read local clock", error))?
            .as_secs()
            .try_into()
            .map_err(|_| StorageError::InvalidTransition("Local clock exceeds i64".into()))?;

        Ok(ChainMetadata {
            schema_version: STORAGE_SCHEMA_VERSION,
            chain_name: "dev".into(),
            protocol_version: 1,
            created_unix_ts,
        })
    }

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

    // Consensus startup separately checks full ancestry and cumulative work.
    fn verify_initialized_state(&self) -> Result<(), StorageError> {
        let view = self.read()?;
        view.verify_default_table_empty()?;

        let Some((metadata, state)) = view.verify_initialization_records()? else {
            return Ok(());
        };

        if metadata.schema_version != STORAGE_SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema(metadata.schema_version));
        }

        view.verify_genesis_index()?;
        view.verify_canonical_tip(&state)?;
        view.verify_highest_canonical_height(state.latest_block.0)
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

impl StryiRead<'_> {
    fn verify_default_table_empty(&self) -> Result<(), StorageError> {
        let mut entries = self
            .reader
            .iterator_cf(self.inner.table("default"), IteratorMode::Start);
        let first_entry = entries
            .next()
            .transpose()
            .map_err(|error| error::backend_error("inspect default column family", error))?;
        if first_entry.is_some() {
            return Err(self.report_corruption("Default column family must be empty"));
        }

        Ok(())
    }

    fn verify_initialization_records(
        &self,
    ) -> Result<Option<(ChainMetadata, ChainState)>, StorageError> {
        let metadata = self.metadata()?;
        let state = self.chain_state()?;

        match (metadata, state) {
            (None, None) => {
                self.verify_pre_genesis_tables_empty()?;
                Ok(None)
            }
            (Some(metadata), Some(state)) => Ok(Some((metadata, state))),
            _ => Err(self
                .report_corruption("Genesis metadata and chain state must be committed together")),
        }
    }

    fn verify_pre_genesis_tables_empty(&self) -> Result<(), StorageError> {
        for name in TABLES {
            let mut entries = self
                .reader
                .iterator_cf(self.inner.table(name), IteratorMode::Start);
            let first_entry = entries
                .next()
                .transpose()
                .map_err(|error| error::backend_error(format!("inspect {name}"), error))?;
            if first_entry.is_some() {
                return Err(self.report_corruption(
                    "Storage has data without genesis metadata and chain state",
                ));
            }
        }

        Ok(())
    }

    fn verify_genesis_index(&self) -> Result<(), StorageError> {
        let genesis = self
            .get_block_by_height(0)?
            .ok_or_else(|| self.report_corruption("Initialized storage has no genesis"))?;

        validate_genesis(&genesis).map_err(|error| {
            self.report_corruption(format!("Saved genesis is invalid: {error}"))
        })?;

        let index = self
            .get_block_index(&BlockHash::empty())?
            .ok_or_else(|| self.report_corruption("Genesis index is absent"))?;
        if index.height != 0
            || index.parent_hash != BlockHash::empty()
            || index.chain_work != ChainWork::ZERO
        {
            return Err(self.report_corruption("Genesis index does not match genesis"));
        }

        Ok(())
    }

    fn verify_canonical_tip(&self, state: &ChainState) -> Result<(), StorageError> {
        let tip = self
            .get_block_by_height(state.latest_block.0)?
            .ok_or_else(|| self.report_corruption("Canonical tip height is absent"))?;
        let index = self
            .get_block_index(&state.latest_block.1)?
            .ok_or_else(|| self.report_corruption("Canonical tip index is absent"))?;
        if tip.block_hash() != state.latest_block.1
            || tip.header.height != state.latest_block.0
            || index.height != state.latest_block.0
            || index.parent_hash != tip.header.previous_block_hash
            || index.chain_work != state.chain_work
            || state.latest_block.0.checked_add(1) != Some(state.blocks_count)
        {
            return Err(self.report_corruption("Canonical tip, index and chain state disagree"));
        }

        Ok(())
    }

    fn verify_highest_canonical_height(&self, tip_height: u64) -> Result<(), StorageError> {
        let mut entries = self
            .reader
            .iterator_cf(self.inner.table("heights"), IteratorMode::End);
        let (highest_key, _) = entries
            .next()
            .transpose()
            .map_err(|error| error::backend_error("inspect highest canonical height", error))?
            .ok_or_else(|| {
                self.report_corruption("Initialized storage has no canonical heights")
            })?;

        let highest_key: [u8; 8] = highest_key
            .as_ref()
            .try_into()
            .map_err(|_| self.report_corruption("Invalid canonical height key"))?;
        if u64::from_be_bytes(highest_key) != tip_height {
            return Err(self.report_corruption("Canonical heights extend beyond the saved tip"));
        }

        Ok(())
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

/// Refuse legacy and unrecognized nonempty datadirs before opening them.
/// Returns true for an existing schema-2 RocksDB database, false for an absent or empty directory.
fn check_supported_datadir(path: &Path) -> Result<bool, StorageError> {
    if !path.exists() {
        return Ok(false);
    }

    let mut entries =
        fs::read_dir(path).map_err(|error| error::backend_error("inspect datadir", error))?;
    let first_entry = entries
        .next()
        .transpose()
        .map_err(|error| error::backend_error("inspect datadir entry", error))?;
    if first_entry.is_none() {
        return Ok(false);
    }

    if path.join("metainfo.bin").exists()
        || path.join("version").exists()
        || path.join("keyspaces").exists()
    {
        return Err(StorageError::UnsupportedSchema(1));
    }

    if !path.join("CURRENT").is_file() {
        return Err(StorageError::InvalidTransition(
            "Datadir is nonempty and is not a RocksDB chain database; use a fresh directory".into(),
        ));
    }

    let found = Database::list_cf(&Options::default(), path)
        .map_err(|error| error::backend_error("list column families", error))?;

    let mut expected: Vec<_> = TABLES
        .into_iter()
        .chain(["default"])
        .map(str::to_owned)
        .collect();
    let mut found = found;
    expected.sort();
    found.sort();

    if found != expected {
        return Err(StorageError::Corruption(
            "Database column families do not match schema 2".into(),
        ));
    }

    Ok(true)
}
