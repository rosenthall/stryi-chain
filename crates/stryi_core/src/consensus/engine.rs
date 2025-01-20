use crate::block::{meets_difficulty, Block};
use crate::consensus::ConsensusEngine;
use crate::consensus::ConsensusRules;
use crate::error::StryiCoreError;

/// implementation of the ConsensusEngine trait.
pub struct StryiConsensusEngine {
    pub rules: ConsensusRules,
}

impl StryiConsensusEngine {
    /// Constructs a new instance of StryiConsensusEngine with the provided consensus rules.
    pub fn new(rules: ConsensusRules) -> Self {
        Self { rules }
    }
}

impl ConsensusEngine for StryiConsensusEngine {
    type Error = StryiCoreError;

    /// Validates a given block according to consensus rules.
    async fn validate_block(&self, block: &Block) -> Result<(), Self::Error> {
        // 1. check if the block's difficulty in header matches the current rules
        if block.header.difficulty_bits != self.rules.current_difficulty {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: format!(
                    "Block difficulty {} does not match current difficulty {}",
                    block.header.difficulty_bits, self.rules.current_difficulty
                ),
            });
        }

        // 2. Check if block_hash actually returns hash that meets difficulty 
        let block_hash = block.block_hash();
        if !meets_difficulty(&block_hash, block.header.difficulty_bits) {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Block does not meet required difficulty".to_string(),
            });
        }

        // 3. Validate merkle root, rebuild it and check if merkle root matches with provided in block.
        if !block.validate_merkle_root() {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "Merkle root mismatch".to_string(),
            });
        }

        // 4. CHECK UTXOS (?)

        Ok(())
    }


    /// Adjusts the difficulty based on the current chain state if needed.
    /// May return error
    async fn adjust_difficulty(&mut self, chain: &[Block]) -> Result<u8, Self::Error> {

        
        // Get current difficulty and check if going to increase it because difficulty_adjustment_interval_blocks rule 
        let current_difficulty = self.rules.current_difficulty;
        let interval = self.rules.difficulty_adjustment_interval_blocks;


        // If interval reached - incrementing difficulty by one
        if !chain.is_empty() && chain.len() % interval == 0 {
            let new_difficulty = current_difficulty.checked_add(1);

            // Difficulty more than 255 must never happen, but still checking in case
            return match new_difficulty {
                Some(val) => {
                    
                    // Update rules state
                    self.rules.update_difficulty(val);
                    
                    Ok(val)
                },
                None => Err(StryiCoreError::InvalidDifficultyValue {
                    details: "Cannot adjust difficulty, value does not fits in sane (u8) limits".to_string(),
                }),
            }

        }

        
        // If no need of adjusting just return current value 
        Ok(current_difficulty)
    }

    /// Chooses the best chain among multiple forks based on cumulative difficulty or other criteria.
    async fn select_chain(&self, chains: Vec<Vec<Block>>) -> Result<Vec<Block>, Self::Error> {
        // select the first available chain so far 
        // TODO: use cumulative difficulty, which is sum of (2^DIFFICULTY) of each block 
        chains.into_iter().next().ok_or(StryiCoreError::ConsensusChainSelectionFailed {
            details: "No chains available for selection.".to_string(),
        })
    }
}
