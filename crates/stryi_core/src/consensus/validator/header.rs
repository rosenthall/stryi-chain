//! Header-level consensus checks.  
//! These routines require **no** UTXO / transaction context.

use crate::{
    block::{Block, meets_difficulty},
    consensus::ConsensusRules,
    error::StryiCoreError,
};

/// Runs every static header rule.
///
/// 1. difficulty bits match the current target;  
/// 2. hash satisfies Proof-of-Work;  
/// 3. Merkle root matches the transaction list.
pub fn validate_header(block: &Block, rules: &ConsensusRules) -> Result<(), StryiCoreError> {
    verify_difficulty(block, rules)?;
    verify_proof_of_work(block)?;
    verify_merkle_root(block)?;
    Ok(())
}

/// Verifies `header.difficulty_bits` equals `rules.current_difficulty`.
fn verify_difficulty(block: &Block, rules: &ConsensusRules) -> Result<(), StryiCoreError> {
    if block.header.difficulty_bits != rules.current_difficulty {
        return Err(StryiCoreError::ConsensusValidationFailed {
            details: format!(
                "Block difficulty ({}) does not match current difficulty ({})",
                block.header.difficulty_bits, rules.current_difficulty,
            ),
        });
    }
    Ok(())
}

/// Ensures the block hash meets the declared target (genesis is always trusted).
fn verify_proof_of_work(block: &Block) -> Result<(), StryiCoreError> {
    if block.header.is_genesis {
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
