use crate::difficulty::PowParams;
use serde::{Deserialize, Serialize};

/// Global consensus parameters stored in the genesis block.
/// These parameters affect proof-of-work behavior and block-reward emission
/// and remain fixed for the lifetime of the chain.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Eq, PartialEq)]
pub struct ConsensusConsts {
    /// Proof-of-work schedule parameters: target spacing, half-life and hash thresholds.
    pub pow: PowParams,

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
}

impl Default for ConsensusConsts {
    /// **Testing** defaults.
    fn default() -> Self {
        Self {
            pow: PowParams::default(),
            initial_subsidy: 1000,
            decay_interval: 10,
            decay_step: 10,
        }
    }
}

impl ConsensusConsts {
    pub fn new(pow: PowParams, initial_subsidy: u64, decay_interval: u64, decay_step: u64) -> Self {
        Self {
            pow,
            initial_subsidy,
            decay_interval,
            decay_step,
        }
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
