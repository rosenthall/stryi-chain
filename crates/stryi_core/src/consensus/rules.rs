use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ConsensusRules {

    /// Current PoW difficulty, representing the number of leading zero bits required for a block to be considered valid.
    /// See src/block/mining.rs for more details.
    pub(crate) current_difficulty: u8,

    /// Number of blocks generated between each difficulty adjustment.
    /// `1000` will mean that each one thousand of applied blocks will increment `current_difficulty` by one.
    pub(crate) difficulty_adjustment_interval_blocks: usize,
    
    // Todo: Maybe add block_generation_interval_seconds for dynamic difficulty increasing dependently on current blocks arriving time ?
}


impl ConsensusRules {
    /// Constructs a new instance of `ConsensusRules` with the provided parameters.
    ///
    /// # Parameters
    /// - `current_difficulty`: The initial difficulty level (number of leading zero bits required).
    /// - `difficulty_adjustment_interval_blocks`: Number of blocks between each difficulty adjustment.
    ///
    /// # Returns
    /// A new instance of `ConsensusRules`.
    pub fn new(
        current_difficulty: u8,
        difficulty_adjustment_interval_blocks: usize,
    ) -> Self {
        Self {
            current_difficulty,
            difficulty_adjustment_interval_blocks,
        }
    }

    /// Returns the current difficulty level as an 8-bit unsigned integer.
    pub fn current_difficulty(&self) -> u8 {
        self.current_difficulty
    }
    
    /// Returns the number of blocks between difficulty adjustments.
    pub fn difficulty_adjustment_interval_blocks(&self) -> usize {
        self.difficulty_adjustment_interval_blocks
    }

    /// Updates the current difficulty to a new value.
    ///
    /// # Parameters
    /// - `new_difficulty`: The new difficulty level to be set.
    pub fn update_difficulty(&mut self, new_difficulty: u8) {
        self.current_difficulty = new_difficulty;
    }
}
