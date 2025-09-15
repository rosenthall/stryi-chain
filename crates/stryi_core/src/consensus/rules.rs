use serde::{Deserialize, Serialize};


/// Global consensus parameters, covering proof‑of‑work difficulty adjustment and block‑reward emission.
#[derive(Serialize, Deserialize, Debug, Clone)]
// TODO: Refactor `ConsensusRules`, shall be no current_difficulty as fixed number. Maybe consider using `UtxoLookup`-like pattern as a getter for current state?
pub struct ConsensusRules {

    /// Current PoW difficulty, representing the number of leading zero bits required for a block to be considered valid.
    /// See src/block/mining.rs for more details.
    pub(crate) current_difficulty: u8,

    /// Number of blocks generated between each difficulty adjustment.
    /// `1000` will mean that each one thousand of applied blocks will increment `current_difficulty` by one.
    pub(crate) difficulty_adjustment_interval_blocks: usize,

    /// The starting block subsidy (in the chain’s base units) paid to the miner of block *height 1*.
    ///
    /// Example: `10_000` means the genesis‑era reward is 10000 coins.
    pub initial_subsidy: u64,

    /// The number of blocks between **linear reward drops**.
    ///
    /// After every `decay_interval` *applied* blocks, the subsidy is  reduced by `decay_step`.
    /// A value of `0` disables decay (reward remains constant).
    ///
    /// Example: `1000` means the subsidy changes only once every thousand blocks.
    pub decay_interval: u64,


    /// The amount (in coins) by which the subsidy is reduced each time the `decay_interval` is reached.
    pub decay_step:  u64,

    // Todo: Maybe add block_generation_interval_seconds for dynamic difficulty increasing dependently on current blocks arriving time ?
}






impl ConsensusRules {
    
    
    /// *TEMPORARY API*
    /// Constructs new instance of ConsensusRules with provided current_difficulty and other fields are set to some reasonable values.
    /// In the future, this will be deleted and refactored as well as ConsensusRules by itself
    // TODO: Delete `default_with_difficulty()` method for the ConsensusRules, make genesis block actually store basic parameters and chain's settings.
    pub fn default_with_difficulty(current_difficulty: u8) -> Self {
        Self {
            current_difficulty,
            difficulty_adjustment_interval_blocks: 100,
            initial_subsidy: 1000,
            decay_interval: 10,
            decay_step: 10,
        }
    }
    
    
    /// Creates a new set of global consensus parameters.
    ///
    /// # Parameters
    /// * `current_difficulty` – initial PoW difficulty (leading‑zero bits).  
    /// * `difficulty_adjustment_interval_blocks` – how many blocks between difficulty retargets.  
    /// * `initial_subsidy` – reward for block height 0, expressed in the chain’s base units.  
    /// * `decay_interval` – number of blocks between linear reward drops (0 => no decay).
    /// * `decay_step` – amount subtracted from the subsidy each time `decay_interval` is reached.
    pub fn new(
        current_difficulty: u8,
        difficulty_adjustment_interval_blocks: usize,
        initial_subsidy: u64,
        decay_interval: u64,
        decay_step: u64,
    ) -> Self {
        Self {
            current_difficulty,
            difficulty_adjustment_interval_blocks,
            initial_subsidy,
            decay_interval,
            decay_step,
        }
    }
    
    #[cfg(test)]
    /// Creates an instance of ConsensusRules with reasonable values for testing. Current difficulty still should be provided
    pub const fn new_test(current_difficulty: u8) -> Self {
        Self {
            current_difficulty,
            difficulty_adjustment_interval_blocks: 100,
            initial_subsidy: 1000,
            decay_interval: 10,
            decay_step: 10,
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


    /// Computes the block subsidy for a given height **after** linear decay.
    ///
    /// ```text
    /// subsidy(height) = max(0, initial_subsidy − floor(height / decay_interval) × decay_step)
    /// ```
    pub fn block_subsidy(&self, height: u64) -> u64 {
        if self.decay_interval == 0 {
            return self.initial_subsidy;
        }


        let steps = height / self.decay_interval;
        self.initial_subsidy
            .saturating_sub(steps.saturating_mul(self.decay_step))
    }

    /// Gets current `initial_subsidy`
    pub fn initial_subsidy(&self) -> u64 {
        self.initial_subsidy 
    }
  
    /// Gets current `decay_interval`
    pub fn decay_interval(&self) -> u64 { 
        self.decay_interval 
    }
    
    /// Gets current `decay_step`
    pub fn decay_step(&self) -> u64 {
        self.decay_step
    }

}
