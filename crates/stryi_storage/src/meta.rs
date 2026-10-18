use crate::{StryiStorage, StryiStorageError};
use serde::{Deserialize, Serialize};
use stryi_core::storage::{StorageReader, StorageStats};

/// Version of the RocksDB layout documented in `TABLES.md`.
pub const STORAGE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum StorageStatus {
    /// No committed genesis yet; initialized metadata and chain state are absent together.
    NoGenesis,
    /// Storage has a committed genesis and carries essential metadata.
    Initialized {
        chain_name: String,
        protocol_version: u64,
        created_unix_ts: i64,
    },
}

impl StryiStorage {
    /// Probe metadata from the open database. Unreadable data is an error rather
    /// than an uninitialized status, so bootstrap cannot overwrite a broken chain.
    pub fn status(&self) -> Result<StorageStatus, StryiStorageError> {
        match self.read()?.metadata()? {
            None => Ok(StorageStatus::NoGenesis),
            Some(metadata) => Ok(StorageStatus::Initialized {
                chain_name: metadata.chain_name,
                protocol_version: metadata.protocol_version,
                created_unix_ts: metadata.created_unix_ts,
            }),
        }
    }
}
