use crate::{
    ChainMetadata, Database, GenesisInitConfig, STORAGE_SCHEMA_VERSION, StorageInner, StryiRead,
    StryiStorage, TABLES, error, validate_genesis,
};
use rocksdb::{
    ColumnFamilyDescriptor, DBCompressionType, IteratorMode, Options, TransactionDBOptions,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, atomic::AtomicBool};
use std::time::{SystemTime, UNIX_EPOCH};
use stryi_core::block::{Block, BlockHash};
use stryi_core::difficulty::ChainWork;
use stryi_core::storage::{
    BlockStorage, ChainState, ChainTransition, FullNodeStorage, StorageError, StorageReader,
    StorageStats,
};
use tracing::info;

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
