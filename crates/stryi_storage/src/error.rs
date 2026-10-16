//! Backend errors use the common core contract so consensus does not depend on
//! RocksDB. Keep the existing public name while preserving backend error sources.

pub use stryi_core::storage::StorageError as StryiStorageError;

use std::error::Error;
use std::sync::Arc;
use stryi_core::storage::StorageError;

pub(crate) fn backend_error(
    operation: impl Into<String>,
    source: impl Error + Send + Sync + 'static,
) -> StorageError {
    StorageError::Backend {
        operation: operation.into(),
        source: Arc::new(source),
    }
}
