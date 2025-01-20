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
    


    /// Computes the total (cumulative) difficulty of a chain by summing 2^(bits)
    /// for each block. TODO: // big int here?
    fn compute_chain_difficulty(&self, chain: &[Block]) -> u128 {
        let mut total: u128 = 0;

        // Each block's difficulty_bits is a u8
        // 2^(bits) can be computed as 1 << bits if bits < 128
        for block in chain {
            let bits = block.header.difficulty_bits;
            // naive approach (assuming bits < 128 to avoid shift overflow)
            let block_work = 1u128 << bits;
            total = total.saturating_add(block_work);
        }

        total
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

    /// Chooses the best chain among multiple forks based on cumulative difficulty 
    async fn select_chain(&self, chains: Vec<Vec<Block>>) -> Result<Vec<Block>, Self::Error> {
        // Use max_by_key to select the chain with the highest cumulative difficulty.
        // If no chains are provided, return an error.
        chains
            .into_iter()
            .max_by_key(|chain| self.compute_chain_difficulty(chain))
            .ok_or_else(|| StryiCoreError::ConsensusChainSelectionFailed {
                details: "No chains available for selection.".to_string(),
            })
    }

}



#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{Block, BlockHeader, BlockData, BlockHash};

    /// Helper that fabricates a block with given `difficulty_bits`.
    fn make_block(difficulty_bits: u8) -> Block {
        // For testing, we skip real mining. We'll just define a block with a known difficulty.
        let header = BlockHeader {
            version: 1,
            merkle_root_hash: [0u8; 32], // dummy
            previous_block_hash: BlockHash::empty(),
            height: 0,
            difficulty_bits,
            timestamp: 0,
            nonce: 0,
        };
        let data = BlockData { transactions: vec![] };
        Block { header, data }
    }

    #[tokio::test]
    async fn test_select_chain_by_cumulative_difficulty() {
        let rules = ConsensusRules::new(4, 1000); // example
        let engine = StryiConsensusEngine::new(rules);

        // Make short chains with artificially assigned difficulty
        // chainA: blocks => bits=4, 4 => total ~ 2^4 + 2^4 = 32
        let chain_a = vec![
            make_block(4),
            make_block(4),
        ];

        // chainB: blocks => bits=5, 3 => total ~ 2^5 * 3 = 96
        let chain_b = vec![
            make_block(5),
            make_block(5),
            make_block(5),
        ];

        // chainC: blocks => bits=1, 10 => total ~ 2^1 * 10 = 20
        let chain_c = vec![
            make_block(1); // repeated 10 times
            10
        ];

        // The engine picks the chain with the highest total difficulty => chainB in this example
        let best_chain = engine
            .select_chain(vec![chain_a.clone(), chain_b.clone(), chain_c.clone()])
            .await
            .expect("Should find a best chain");

        // Check that it's indeed chainB (the one with bits=5)
        let best_diff = engine.compute_chain_difficulty(&best_chain);
        let b_diff = engine.compute_chain_difficulty(&chain_b);
        assert_eq!(best_diff, b_diff, "select_chain did not pick chain B");
    }
}
