use crate::{ReadSource, StorageView};
use stryi_core::BlockUndo;
use stryi_core::block::BlockHash;
use stryi_core::storage::{StorageError, UndoStorage};

impl<R: ReadSource> UndoStorage for StorageView<'_, R> {
    fn get_block_undo(&self, hash: BlockHash) -> Result<Option<BlockUndo>, StorageError> {
        self.get("undo", &hash.data)
    }
}
