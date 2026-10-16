use crate::block::Block;
use crate::difficulty::{
    DifficultyCalc, HashThreshold, MAX_FUTURE_BLOCK_TIME_SECS, median_time_past,
};
use crate::error::StryiCoreError;
use crate::storage::{BlockStorage, verify_genesis_work_shape};
use tracing::trace;

/// Runs all header-level consensus rules.
pub fn validate_header<STATE>(
    block: &Block,
    calc: &DifficultyCalc,
    state: &STATE,
) -> Result<(), StryiCoreError>
where
    STATE: BlockStorage + Send + Sync,
{
    let hash = block.block_hash().to_string();
    trace!("Validating header for block hash {}", hash);

    verify_header_shape(block)?;
    trace!("Block {} passed header shape verification", hash);

    if !block.header.is_genesis() {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| StryiCoreError::other(e.to_string()))?
            .as_secs();

        verify_timestamp(block, state, now_secs)?;
    }

    verify_hash_threshold(block, calc, state)?;
    trace!(
        "Block {} passed hash threshold verification (matches the expected threshold for this parent)",
        hash
    );

    verify_proof_of_work(block)?;
    trace!("Block {} passed proof of work check", hash);

    verify_merkle_root(block)?;
    trace!("Block {} passed merkle root validation", hash);

    Ok(())
}

/// Verifies genesis shape and requires a positive threshold for normal blocks.
fn verify_header_shape(block: &Block) -> Result<(), StryiCoreError> {
    verify_genesis_work_shape(block)?;

    if !block.header.is_genesis() && block.header.hash_threshold == HashThreshold::ZERO {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "normal block must carry a positive hash threshold (ZERO is genesis-only)"
                .into(),
        });
    }

    Ok(())
}

fn verify_timestamp<STATE>(
    block: &Block,
    state: &STATE,
    now_secs: u64,
) -> Result<(), StryiCoreError>
where
    STATE: BlockStorage + Send + Sync,
{
    let median = median_time_past(state, block.header.previous_block_hash)?;
    if block.header.timestamp <= median {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "block timestamp must be greater than median time past".into(),
        });
    }

    if block.header.timestamp > now_secs.saturating_add(MAX_FUTURE_BLOCK_TIME_SECS) {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "block timestamp exceeds future time limit".into(),
        });
    }

    Ok(())
}

/// Verifies `header.hash_threshold` equals the expected value for the block.
fn verify_hash_threshold<STATE>(
    block: &Block,
    calc: &DifficultyCalc,
    state: &STATE,
) -> Result<(), StryiCoreError>
where
    STATE: BlockStorage + Send + Sync,
{
    if block.header.is_genesis() {
        return Ok(());
    }

    let expected = calc.next_hash_threshold(state, block.header.previous_block_hash)?;

    if block.header.hash_threshold != expected {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: format!(
                "Invalid hash threshold: expected {}, got {}",
                expected, block.header.hash_threshold,
            ),
        });
    }

    Ok(())
}

/// Ensures the block hash meets the declared threshold.
fn verify_proof_of_work(block: &Block) -> Result<(), StryiCoreError> {
    if block.header.is_genesis() {
        return Ok(());
    }

    let hash = block.block_hash();

    if !hash.meets_threshold(&block.header.hash_threshold) {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "Block does not meet required Proof-of-Work threshold".into(),
        });
    }

    Ok(())
}

/// Recomputes the Merkle root and compares with the header value.
fn verify_merkle_root(block: &Block) -> Result<(), StryiCoreError> {
    let calc = Block::compute_merkle_root(&block.data.transactions);

    if block.header.merkle_root_hash != calc {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "Merkle root mismatch".into(),
        });
    }

    Ok(())
}
