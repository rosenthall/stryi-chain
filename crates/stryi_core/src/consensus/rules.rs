use serde::{Deserialize, Serialize};

/// Global consensus parameters stored in the genesis block.
/// These parameters affect proof‑of‑work difficulty adjustment and block‑reward emission
/// and remain fixed for the lifetime of the chain.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Eq, PartialEq)]
pub struct ConsensusConsts {
    /// Number of blocks generated between each difficulty adjustment.
    /// `1000` will mean that each one thousand of applied blocks will increment `current_difficulty` by one.
    pub difficulty_adjustment_interval_blocks: u64,

    /// The starting block subsidy paid to the miner of block *height 1*.
    ///
    /// Example: `10_000` means the genesis-era reward is 10,000 coins.
    pub initial_subsidy: u64,

    /// The number of blocks between **linear reward drops**.
    ///
    /// After every `decay_interval` *applied* blocks, the subsidy is  reduced by `decay_step`.
    /// A value of `0` disables decay (reward remains constant).
    ///
    /// Example: `1000` means the subsidy changes only once every thousand blocks.
    pub decay_interval: u64,

    /// The amount (in coins) by which the subsidy is reduced each time the `decay_interval` is reached.
    pub decay_step: u64,
    // Todo: Maybe add block_generation_interval_seconds for dynamic difficulty increasing dependently on current blocks arriving time ?
}

impl Default for ConsensusConsts {
    /// **Testing** defaults.
    fn default() -> Self {
        Self {
            difficulty_adjustment_interval_blocks: 100,
            initial_subsidy: 1000,
            decay_interval: 10,
            decay_step: 10,
        }
    }
}

impl ConsensusConsts {
    pub fn new(
        difficulty_adjustment_interval_blocks: u64,
        initial_subsidy: u64,
        decay_interval: u64,
        decay_step: u64,
    ) -> Self {
        Self {
            difficulty_adjustment_interval_blocks,
            initial_subsidy,
            decay_interval,
            decay_step,
        }
    }

    /// Height-only difficulty: genesis=0; from height>=1 start at 1 bit and
    /// increase by +1 every `difficulty_adjustment_interval_blocks`.
    /// If the interval is 0, `difficulty` stays at 1 for all non-genesis heights.
    #[inline]
    pub fn difficulty_bits_for_height(&self, height: u64) -> u8 {
        if height == 0 {
            return 0;
        }

        // Treat interval `0` as "no retarget": stay at 1.
        let interval = self.difficulty_adjustment_interval_blocks.max(1);

        let steps = (height.saturating_sub(1)) / interval; // 0,1,2,...

        // 1 + steps, clamped to 255 so no overflows are possible
        let bits_u16 = 1u16.saturating_add(steps as u16);
        bits_u16.min(255) as u8
    }

    /// Computes the block subsidy for a given height **after** linear decay.
    ///
    /// ```text
    /// subsidy(height) = max(0, initial_subsidy − floor(height / decay_interval) * decay_step)
    /// ```
    #[inline]
    pub fn block_subsidy(&self, height: u64) -> u64 {
        if self.decay_interval == 0 {
            return self.initial_subsidy;
        }

        let steps = height / self.decay_interval;
        self.initial_subsidy
            .saturating_sub(steps.saturating_mul(self.decay_step))
    }
}
