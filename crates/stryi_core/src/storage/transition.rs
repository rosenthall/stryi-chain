use super::{
    BlockIndexData, ChainMetadata, ChainState, ChainTransition, ChainTransitionResult,
    StorageError, StorageRead, StorageWrite, verify_genesis_work_shape,
};
use crate::block::{Block, BlockHash};
use crate::consensus::ForkDbDelta;
use crate::difficulty::{ChainWork, HashThreshold, add_work};
use crate::transactions::{TransactionKind, UtxoProcessor};
use std::collections::HashSet;

struct StagedTransition {
    state: ChainState,
    detached: Vec<(u64, BlockHash)>,
}

/// Stages a chain transition inside the provided write view (without committing).
pub fn apply_transition<S: StorageWrite>(
    storage: &mut S,
    transition: ChainTransition<'_>,
) -> Result<ChainTransitionResult, StorageError> {
    let current = storage.chain_state()?;

    let staged = match transition {
        ChainTransition::Genesis { block, metadata } => {
            stage_genesis(storage, current, block, metadata)?
        }
        ChainTransition::Append { block } => stage_append(storage, current, block)?,
        ChainTransition::Reorganize { delta } => stage_reorganization(storage, current, delta)?,
    };

    Ok(ChainTransitionResult {
        new_head: staged.state.latest_block,
        chain_work: staged.state.chain_work,
        detached: staged.detached,
    })
}

fn stage_genesis<S: StorageWrite>(
    storage: &mut S,
    current: Option<ChainState>,
    block: &Block,
    metadata: &ChainMetadata,
) -> Result<StagedTransition, StorageError> {
    validate_genesis(block)?;

    if let Some(state) = current {
        let saved = storage
            .get_block_by_height(0)?
            .ok_or_else(|| StorageError::Corruption("Initialized storage has no genesis".into()))?;
        let saved_metadata = storage.metadata()?.ok_or_else(|| {
            StorageError::Corruption("Initialized storage has no metadata".into())
        })?;

        // Genesis hash is always zero, so check full block and chain params.
        if saved != *block
            || saved_metadata.schema_version != metadata.schema_version
            || saved_metadata.chain_name != metadata.chain_name
            || saved_metadata.protocol_version != metadata.protocol_version
        {
            return Err(StorageError::InvalidTransition(
                "Genesis or chain configuration does not match stored chain".into(),
            ));
        }

        verify_current_state(storage, &state)?;

        return Ok(StagedTransition {
            state,
            detached: Vec::new(),
        });
    }

    if storage.metadata()?.is_some() || storage.get_block_by_height(0)?.is_some() {
        return Err(StorageError::Corruption(
            "Genesis exists without chain state".into(),
        ));
    }

    let index = BlockIndexData {
        parent_hash: block.header.previous_block_hash,
        height: 0,
        chain_work: ChainWork::ZERO,
    };

    storage.put_block(block, &index)?;
    UtxoProcessor::new().apply_block(block, storage)?;
    storage.put_metadata(metadata)?;

    let staged = StagedTransition {
        state: ChainState {
            latest_block: (0, block.block_hash()),
            last_update_time: block.header.timestamp,
            blocks_count: 1,
            chain_work: ChainWork::ZERO,
        },
        detached: Vec::new(),
    };

    storage.put_chain_state(&staged.state)?;

    Ok(staged)
}

fn stage_append<S: StorageWrite>(
    storage: &mut S,
    current: Option<ChainState>,
    block: &Block,
) -> Result<StagedTransition, StorageError> {
    let mut state = current.ok_or(StorageError::Uninitialized)?;
    verify_current_state(storage, &state)?;

    attach_block(storage, &UtxoProcessor::new(), &mut state, block)?;

    let staged = StagedTransition {
        state,
        detached: Vec::new(),
    };

    storage.put_chain_state(&staged.state)?;

    Ok(staged)
}

fn stage_reorganization<S: StorageWrite>(
    storage: &mut S,
    current: Option<ChainState>,
    delta: ForkDbDelta,
) -> Result<StagedTransition, StorageError> {
    let mut state = current.ok_or(StorageError::Uninitialized)?;
    verify_current_state(storage, &state)?;

    // Reject if canonical tip moved while the overlay was being prepared
    if state.latest_block != delta.expected_head {
        return Err(StorageError::InvalidTransition(
            "Canonical tip changed after fork preparation".into(),
        ));
    }

    let (ancestor, ancestor_index) = canonical_ancestor(storage, delta.common_ancestor)?;
    let detached = detach_to_ancestor(storage, &mut state, &ancestor, delta.common_ancestor)?;

    if detached != delta.detached || delta.blocks.is_empty() {
        return Err(StorageError::InvalidTransition(
            "Prepared fork does not replace the expected canonical suffix".into(),
        ));
    }

    // Start from ancestor's work; UTXOs and undo were already computed in the overlay
    state.chain_work = ancestor_index.chain_work;
    storage.batch_remove_utxos(delta.removed_utxos)?;
    storage.batch_put_utxos(delta.utxos)?;

    for (block, undo) in delta.blocks {
        state = extended_state(&state, &block)?;
        let index = BlockIndexData {
            parent_hash: block.header.previous_block_hash,
            height: block.header.height,
            chain_work: state.chain_work,
        };

        storage.put_block(&block, &index)?;
        storage.put_block_undo(block.block_hash(), undo)?;
    }

    storage.put_chain_state(&state)?;

    Ok(StagedTransition { state, detached })
}

fn canonical_ancestor<S: StorageRead>(
    storage: &S,
    common_ancestor: BlockHash,
) -> Result<(Block, BlockIndexData), StorageError> {
    let ancestor = storage
        .get_block_by_hash(common_ancestor)?
        .ok_or(StorageError::BlockNotFound(common_ancestor))?;
    let canonical = storage
        .get_block_hash_by_height(ancestor.header.height)?
        .ok_or_else(|| StorageError::Corruption("Common ancestor height is absent".into()))?;

    if ancestor.block_hash() != common_ancestor {
        return Err(StorageError::Corruption(
            "Common ancestor body does not match its key".into(),
        ));
    }

    if canonical != common_ancestor {
        // A different valid canonical block rejects the request. A broken height
        // reference is storage corruption, even when this ancestor is detached.
        let canonical_block = storage.get_block_by_hash(canonical)?.ok_or_else(|| {
            StorageError::Corruption("Common ancestor height body is absent".into())
        })?;
        if canonical_block.header.height != ancestor.header.height {
            return Err(StorageError::Corruption(
                "Common ancestor height references a block at another height".into(),
            ));
        }

        return Err(StorageError::InvalidTransition(
            "Common ancestor is outside the canonical chain".into(),
        ));
    }

    let index = storage
        .get_block_index(&common_ancestor)?
        .ok_or_else(|| StorageError::Corruption("Common ancestor index is absent".into()))?;

    if index.height != ancestor.header.height
        || index.parent_hash != ancestor.header.previous_block_hash
    {
        return Err(StorageError::Corruption(
            "Common ancestor index does not match its block".into(),
        ));
    }

    Ok((ancestor, index))
}

fn detach_to_ancestor<S: StorageWrite>(
    storage: &mut S,
    state: &mut ChainState,
    ancestor: &Block,
    common_ancestor: BlockHash,
) -> Result<Vec<(u64, BlockHash)>, StorageError> {
    let mut detached = Vec::new();

    // Remove heights and tx indexes tip-first; block bodies and undo stay by hash
    while state.latest_block.1 != common_ancestor {
        if state.latest_block.0 <= ancestor.header.height {
            return Err(StorageError::InvalidTransition(
                "Common ancestor is not in tip ancestry".into(),
            ));
        }

        let hash = state.latest_block.1;
        let block = storage
            .get_block_by_hash(hash)?
            .ok_or_else(|| StorageError::Corruption(format!("Canonical block {hash} is absent")))?;
        let canonical = storage
            .get_block_hash_by_height(state.latest_block.0)?
            .ok_or_else(|| {
                StorageError::Corruption(format!(
                    "Canonical height {} is absent",
                    state.latest_block.0
                ))
            })?;

        if block.block_hash() != hash
            || block.header.height != state.latest_block.0
            || canonical != hash
        {
            return Err(StorageError::Corruption(
                "Canonical tip mappings disagree".into(),
            ));
        }

        storage.remove_canonical_block(&block)?;

        state.latest_block = (block.header.height - 1, block.header.previous_block_hash);
        detached.push((block.header.height, hash));
    }

    Ok(detached)
}

/// Check the records needed to stage a transition, using that same storage view.
/// Backend startup checks layout, schema and genesis; consensus startup checks
/// the complete canonical ancestry and cumulative work.
fn verify_current_state<S: StorageRead>(
    storage: &S,
    state: &ChainState,
) -> Result<(), StorageError> {
    if storage.metadata()?.is_none() {
        return Err(StorageError::Corruption(
            "Initialized storage has no metadata".into(),
        ));
    }

    let block = storage
        .get_block_by_hash(state.latest_block.1)?
        .ok_or_else(|| StorageError::Corruption("Canonical tip body is absent".into()))?;
    let canonical = storage
        .get_block_hash_by_height(state.latest_block.0)?
        .ok_or_else(|| StorageError::Corruption("Canonical tip height is absent".into()))?;
    let index = storage
        .get_block_index(&state.latest_block.1)?
        .ok_or_else(|| StorageError::Corruption("Canonical tip index is absent".into()))?;

    if block.block_hash() != state.latest_block.1
        || block.header.height != state.latest_block.0
        || canonical != state.latest_block.1
        || index.height != state.latest_block.0
        || index.parent_hash != block.header.previous_block_hash
        || index.chain_work != state.chain_work
        || state.latest_block.0.checked_add(1) != Some(state.blocks_count)
    {
        return Err(StorageError::Corruption(
            "Canonical tip, index and chain state disagree".into(),
        ));
    }

    Ok(())
}

fn attach_block<S: StorageWrite>(
    storage: &mut S,
    processor: &UtxoProcessor,
    state: &mut ChainState,
    block: &Block,
) -> Result<(), StorageError> {
    let next_state = extended_state(state, block)?;
    let index = BlockIndexData {
        parent_hash: block.header.previous_block_hash,
        height: block.header.height,
        chain_work: next_state.chain_work,
    };

    let undo = processor.apply_block(block, storage)?;
    storage.put_block(block, &index)?;
    storage.put_block_undo(block.block_hash(), undo)?;
    *state = next_state;

    Ok(())
}

/// Both append and prepared reorg must extend the selected prefix and inherit
/// its cumulative work. This calculation does not apply transactions or write.
fn extended_state(state: &ChainState, block: &Block) -> Result<ChainState, StorageError> {
    verify_genesis_work_shape(block)
        .map_err(|error| StorageError::InvalidTransition(error.to_string()))?;

    if block.header.is_genesis()
        || block.header.hash_threshold == HashThreshold::ZERO
        || block.header.previous_block_hash != state.latest_block.1
        || state.latest_block.0.checked_add(1) != Some(block.header.height)
    {
        return Err(StorageError::InvalidTransition(
            "Block must extend the current canonical tip".into(),
        ));
    }

    let chain_work = add_work(state.chain_work, block.work())
        .map_err(|error| StorageError::InvalidTransition(error.to_string()))?;
    let blocks_count = block
        .header
        .height
        .checked_add(1)
        .ok_or_else(|| StorageError::InvalidTransition("Block count overflow".into()))?;

    Ok(ChainState {
        latest_block: (block.header.height, block.block_hash()),
        last_update_time: block.header.timestamp,
        blocks_count,
        chain_work,
    })
}

/// Validates genesis shape, allocations and PoW parameters before storage.
/// Apply this even on repeated initialization, before trusting the genesis shortcut.
pub fn validate_genesis(block: &Block) -> Result<(), StorageError> {
    verify_genesis_work_shape(block)
        .map_err(|error| StorageError::InvalidTransition(error.to_string()))?;

    let Some(genesis) = block.header.genesis_state else {
        return Err(StorageError::InvalidTransition(
            "Genesis marker is absent".into(),
        ));
    };

    genesis
        .consensus_consts
        .pow
        .validate()
        .map_err(|error| StorageError::InvalidTransition(error.to_string()))?;

    if !block.is_merkle_root_valid() || block.data.transactions.len() != 1 {
        return Err(StorageError::InvalidTransition(
            "Genesis must have a valid merkle root and one transaction".into(),
        ));
    }

    let transaction = &block.data.transactions[0];

    if transaction.data.kind != TransactionKind::Genesis
        || !transaction.data.inputs.is_empty()
        || transaction.data.outputs.is_empty()
    {
        return Err(StorageError::InvalidTransition(
            "Genesis must have one allocation transaction without inputs".into(),
        ));
    }

    let mut recipients = HashSet::new();
    for output in &transaction.data.outputs {
        if output.value == 0 || !recipients.insert(output.recipient) {
            return Err(StorageError::InvalidTransition(
                "Genesis allocations must be positive with unique recipients".into(),
            ));
        }
    }

    Ok(())
}
