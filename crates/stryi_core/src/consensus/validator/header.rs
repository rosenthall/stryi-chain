use crate::block::Block;
use crate::difficulty::{DifficultyCalc, HashThreshold};
use crate::error::StryiCoreError;
use crate::storage::{BlockStorage, verify_genesis_work_shape};
use tracing::trace;

/// Runs all header-level consensus rules.
/// NOTE:
/// - This function is async because difficulty calculation may depend
///   on chain state and require async access.
pub async fn validate_header<STATE>(
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

    verify_hash_threshold(block, calc, state).await?;
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

/// Verifies `header.hash_threshold` equals the expected value for the block.
async fn verify_hash_threshold<STATE>(
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

    let expected = calc
        .next_hash_threshold(state, block.header.previous_block_hash)
        .await?;

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
