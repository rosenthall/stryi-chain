use crate::{METADATA_KEY, ReadSource, STATE_KEY, StorageView};
use stryi_core::storage::{ChainMetadata, ChainState, StorageError, StorageStats};

impl<R: ReadSource> StorageStats for StorageView<'_, R> {
    fn chain_state(&self) -> Result<Option<ChainState>, StorageError> {
        self.get("stats", &STATE_KEY)
    }

    fn metadata(&self) -> Result<Option<ChainMetadata>, StorageError> {
        self.get("metadata", METADATA_KEY)
    }
}
