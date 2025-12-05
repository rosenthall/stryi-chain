//! Header-level consensus checks.  
//! These routines require **no** UTXO / transaction context.

use crate::{
    block::{Block, meets_difficulty},
    consensus::ConsensusConsts,
    error::StryiCoreError,
};
use tracing::trace;

/// Runs every static header rule.
///
/// 1. difficulty bits match the current target;  
/// 2. hash satisfies Proof-of-Work;  
/// 3. Merkle root matches the transaction list.
pub fn validate_header(block: &Block, rules: &ConsensusConsts) -> Result<(), StryiCoreError> {
    let hash = block.block_hash().to_string();
    trace!("Validating header for block hash {}", hash);

    verify_difficulty(block, rules)?;
    trace!("Block {} passed difficulty verification", hash);

    verify_proof_of_work(block)?;
    trace!("Block {} passed proof of work check", hash);

    verify_merkle_root(block)?;
    trace!("Block {} passed merkle root validation", hash);
    Ok(())
}

/// Verifies `header.difficulty_bits` equals to the expected value for the block height.
fn verify_difficulty(block: &Block, rules: &ConsensusConsts) -> Result<(), StryiCoreError> {
    let current_difficulty_bits = rules.difficulty_bits_for_height(block.header.height);

    if block.header.difficulty_bits != current_difficulty_bits {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: format!(
                "Block difficulty ({}) does not match current difficulty ({})",
                block.header.difficulty_bits, current_difficulty_bits,
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
