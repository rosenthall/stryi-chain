use crate::difficulty::DifficultyCalc;
use crate::storage::UtxoStorage;
use crate::{
    block::{Block, meets_difficulty},
    error::StryiCoreError,
};
use tracing::trace;

/// Runs all header-level consensus rules.
/// NOTE:
/// - This function is async because difficulty calculation may depend
///   on chain state and require async access.
pub async fn validate_header<DB>(
    block: &Block,
    calc: &DifficultyCalc<DB>,
    state: &DB,
) -> Result<(), StryiCoreError>
where
    DB: UtxoStorage + Send + Sync + 'static,
{
    let hash = block.block_hash().to_string();
    trace!("Validating header for block hash {}", hash);

    verify_difficulty_bits(block, calc, state).await?;
    trace!(
        "Block {} passed difficulty verification (the difficulty number is reasonable for current state)",
        hash
    );

    verify_proof_of_work(block)?;
    trace!("Block {} passed proof of work check", hash);

    verify_merkle_root(block)?;
    trace!("Block {} passed merkle root validation", hash);
    Ok(())
}

/// Verifies `header.difficulty_bits` equals to the expected value for the block height.
async fn verify_difficulty_bits<STATE>(
    block: &Block,
    difficulty_calc: &DifficultyCalc<STATE>,
    state: &STATE,
) -> Result<(), StryiCoreError>
where
    STATE: Send + Sync,
{
    let expected_bits = (difficulty_calc)(state, block.header.height).await?;

    if block.header.difficulty_bits != expected_bits {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: format!(
                "Invalid difficulty bits: expected {}, got {}",
                expected_bits, block.header.difficulty_bits,
            ),
        });
    }

    Ok(())
}

/// Ensures the block hash meets the declared target (genesis is always trusted).
fn verify_proof_of_work(block: &Block) -> Result<(), StryiCoreError> {
    if block.header.is_genesis() {
        return Ok(());
    }
    let hash = block.block_hash();
    if !meets_difficulty(&hash, block.header.difficulty_bits) {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: "Block does not meet required Proof-of-Work difficulty".into(),
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
