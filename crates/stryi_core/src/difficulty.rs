use crate::StryiCoreError;
use crate::block::{Block, BlockHash, BlockHeader};
use crate::consensus::ConsensusConsts;
use crate::error::StorageLayer;
use crate::storage::{BlockStorage, StorageStats, verify_genesis_work_shape};
use crypto_bigint::{CheckedAdd, NonZero};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

mod asert;
pub use asert::asert_upper;

pub use crypto_bigint::U256 as HashThreshold;
pub use crypto_bigint::U512 as ChainWork;

pub const MAX_FUTURE_BLOCK_TIME_SECS: u64 = 120;

async fn read_required_block<S>(state: &S, hash: BlockHash) -> Result<Block, StryiCoreError>
where
    S: BlockStorage + Send + Sync,
{
    let block = state
        .get_block_by_hash(hash)
        .await
        .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))?
        .ok_or_else(|| StryiCoreError::ConsensusValidationFailed {
            details: format!("block {hash} is missing from the branch view"),
        })?;

    if block.block_hash() != hash {
        return Err(StryiCoreError::storage(
            StorageLayer::Block,
            format!("block hash does not match storage key {hash}"),
        ));
    }

    Ok(block)
}

async fn read_required_block_by_height<S>(state: &S, height: u64) -> Result<Block, StryiCoreError>
where
    S: BlockStorage + Send + Sync,
{
    let block = state
        .get_block_by_height(height)
        .await
        .map_err(|e| StryiCoreError::storage(StorageLayer::Block, e.to_string()))?
        .ok_or_else(|| StryiCoreError::ConsensusValidationFailed {
            details: format!("block at height {height} is missing from the branch view"),
        })?;

    if block.header.height != height {
        return Err(StryiCoreError::storage(
            StorageLayer::Block,
            format!("block height does not match storage key {height}"),
        ));
    }

    Ok(block)
}

/// Upper median of up to eleven ancestors, starting at the parent.
pub async fn median_time_past<S>(state: &S, parent_hash: BlockHash) -> Result<u64, StryiCoreError>
where
    S: BlockStorage + Send + Sync,
{
    let mut timestamps = [0u64; 11];
    let mut count = 0;
    let mut cursor = read_required_block(state, parent_hash).await?;

    loop {
        verify_genesis_work_shape(&cursor)?;
        if !cursor.header.is_genesis() && cursor.header.height == 0 {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "normal ancestor has height zero".into(),
            });
        }

        timestamps[count] = cursor.header.timestamp;
        count += 1;
        if cursor.header.is_genesis() || count == timestamps.len() {
            break;
        }

        let previous = read_required_block(state, cursor.header.previous_block_hash).await?;
        if previous.header.height.checked_add(1) != Some(cursor.header.height) {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "broken timestamp ancestry".into(),
            });
        }

        cursor = previous;
    }

    timestamps[..count].sort_unstable();
    Ok(timestamps[count / 2])
}

// H <= target accepts target + 1 hashes, so work is
// floor(2^256 / (target + 1)), the inverse success probability.
pub fn block_work(target: &HashThreshold) -> ChainWork {
    let space = ChainWork::ONE.shl_vartime(256);
    // Widen before adding one so HashThreshold::MAX + 1 does not wrap.
    let target: ChainWork = target.resize();
    let divisor = NonZero::<ChainWork>::new_unwrap(target + ChainWork::ONE);
    space / divisor
}

pub fn add_work(parent: ChainWork, work: ChainWork) -> Result<ChainWork, StryiCoreError> {
    parent
        .checked_add(&work)
        .into_option()
        .ok_or_else(|| StryiCoreError::consensus_chain_selection("chain work overflow"))
}

// Numeric wire codecs stay little-endian, independently of the big-endian
// raw hash digest used for the PoW comparison.
pub fn threshold_from_bytes(bytes: &[u8]) -> Result<HashThreshold, &'static str> {
    if bytes.len() != 32 {
        return Err("hash_threshold must contain exactly 32 bytes");
    }
    Ok(HashThreshold::from_le_slice(bytes))
}

pub fn threshold_to_bytes(value: &HashThreshold) -> [u8; 32] {
    value.to_le_bytes().into()
}

pub fn chain_work_from_bytes(bytes: &[u8]) -> Result<ChainWork, &'static str> {
    if bytes.len() != 64 {
        return Err("chain_work must contain exactly 64 bytes");
    }
    Ok(ChainWork::from_le_slice(bytes))
}

pub fn chain_work_to_bytes(value: &ChainWork) -> [u8; 64] {
    value.to_le_bytes().into()
}

/// Proof-of-work schedule parameters stored in genesis and fixed for the chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowParams {
    /// Target interval between blocks, in seconds.
    pub target_spacing_secs: u32,

    /// Time in which the difficulty target doubles when a branch falls behind schedule.
    pub half_life_secs: u32,

    /// Hash threshold for the first normal blocks of a branch.
    pub initial_hash_threshold: HashThreshold,

    /// Easiest allowed hash threshold - the upper bound of the schedule domain.
    pub max_hash_threshold: HashThreshold,
}

impl PowParams {
    /// Rejects parameter sets that cannot drive a valid difficulty schedule.
    pub fn validate(&self) -> Result<(), StryiCoreError> {
        if self.target_spacing_secs == 0 {
            return Err(StryiCoreError::InvalidDifficultyValue {
                details: "target_spacing_secs must be positive".into(),
            });
        }
        if self.half_life_secs == 0 {
            return Err(StryiCoreError::InvalidDifficultyValue {
                details: "half_life_secs must be positive".into(),
            });
        }
        if self.initial_hash_threshold == HashThreshold::ZERO {
            return Err(StryiCoreError::InvalidDifficultyValue {
                details: "initial_hash_threshold must be positive".into(),
            });
        }
        if self.max_hash_threshold == HashThreshold::ZERO {
            return Err(StryiCoreError::InvalidDifficultyValue {
                details: "max_hash_threshold must be positive".into(),
            });
        }
        if self.initial_hash_threshold > self.max_hash_threshold {
            return Err(StryiCoreError::InvalidDifficultyValue {
                details: "initial_hash_threshold must not exceed max_hash_threshold".into(),
            });
        }
        Ok(())
    }
}

impl Default for PowParams {
    /// **Testing** defaults: 10s spacing, one hour half-life, easiest threshold `MAX >> 1`.
    fn default() -> Self {
        Self {
            target_spacing_secs: 10,
            half_life_secs: 3600,
            initial_hash_threshold: HashThreshold::MAX >> 1,
            max_hash_threshold: HashThreshold::MAX >> 1,
        }
    }
}

/// Difficulty calculator holding the proof-of-work parameters of the chain.
///
/// Thresholds are calculated relative to the branch anchor at height 1 using ASERT.
/// Callers must provide a stable branch view where height lookups consistently
/// reflect the chosen branch history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DifficultyCalc {
    params: PowParams,
}

impl DifficultyCalc {
    /// Builds a calculator from the provided proof-of-work parameters.
    pub fn new(params: PowParams) -> Result<Self, StryiCoreError> {
        params.validate()?;
        Ok(Self { params })
    }

    fn verify_anchor(&self, anchor: &Block) -> Result<(), StryiCoreError> {
        verify_genesis_work_shape(anchor)?;
        if anchor.header.is_genesis() {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "anchor block cannot have genesis shape".into(),
            });
        }
        if anchor.header.height != 1 {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: format!(
                    "anchor block height is {}, expected 1",
                    anchor.header.height
                ),
            });
        }
        if anchor.header.previous_block_hash != BlockHash::empty() {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "anchor block must have empty previous block hash".into(),
            });
        }
        if anchor.header.hash_threshold != self.params.initial_hash_threshold {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "anchor block must use initial hash threshold".into(),
            });
        }
        Ok(())
    }

    /// Returns the hash threshold required for a candidate block whose parent is `parent_hash`.
    ///
    /// The parent and the branch anchor at height 1 are looked up from the provided `state`.
    /// The caller must ensure that `state` provides a consistent view of the branch containing
    /// `parent_hash`. Missing blocks, branch inconsistencies, or invalid anchor shapes return
    /// an error.
    pub async fn next_hash_threshold<S>(
        &self,
        state: &S,
        parent_hash: BlockHash,
    ) -> Result<HashThreshold, StryiCoreError>
    where
        S: BlockStorage + Send + Sync,
    {
        let parent = read_required_block(state, parent_hash).await?;
        verify_genesis_work_shape(&parent)?;

        if parent.header.is_genesis() {
            return Ok(self.params.initial_hash_threshold);
        }

        if parent.header.height == 0 {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: "normal parent has height zero".into(),
            });
        }

        let parent_at_height = read_required_block_by_height(state, parent.header.height).await?;
        if parent_at_height.block_hash() != parent_hash {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: format!(
                    "branch height {} mapped to block {} instead of parent {parent_hash}",
                    parent.header.height,
                    parent_at_height.block_hash()
                ),
            });
        }

        if parent.header.height == 1 {
            self.verify_anchor(&parent)?;
            return Ok(self.params.initial_hash_threshold);
        }

        let anchor = read_required_block_by_height(state, 1).await?;
        self.verify_anchor(&anchor)?;

        if parent.header.height == 2 && parent.header.previous_block_hash != anchor.block_hash() {
            return Err(StryiCoreError::ConsensusValidationFailed {
                details: format!(
                    "parent at height 2 previous block hash {} does not match anchor {}",
                    parent.header.previous_block_hash,
                    anchor.block_hash()
                ),
            });
        }

        let elapsed = i128::from(parent.header.timestamp) - i128::from(anchor.header.timestamp);
        let scheduled =
            i128::from(self.params.target_spacing_secs) * i128::from(parent.header.height - 1);
        let drift = elapsed - scheduled;

        asert_upper(
            self.params.initial_hash_threshold,
            drift,
            self.params.half_life_secs,
            self.params.max_hash_threshold,
        )
        .ok_or_else(|| StryiCoreError::ConsensusDifficultyAdjustmentFailed {
            details: "ASERT calculation returned None".into(),
        })
    }
}

/// Reads consensus rules from genesis and returns a [`DifficultyCalc`].
pub async fn load_rules_and_build_calc<DB>(
    db: Arc<RwLock<DB>>,
) -> Result<DifficultyCalc, StryiCoreError>
where
    DB: BlockStorage + StorageStats + Send + Sync + 'static,
{
    let genesis_opt = {
        let guard = db.read().await;
        guard.get_block_by_height(0).await
    }
    .map_err(|e| StryiCoreError::StorageError {
        layer: StorageLayer::Block,
        err: format!(
            "failed to read genesis while constructing DifficultyCalc: {}",
            e
        ),
    })?;

    let genesis_block = match genesis_opt {
        Some(b) => b,
        None => {
            return Err(StryiCoreError::StorageError {
                layer: StorageLayer::Block,
                err: "genesis block not found (height 0)".to_string(),
            });
        }
    };

    match genesis_block {
        Block {
            header:
                BlockHeader {
                    genesis_state: Some(state),
                    ..
                },
            ..
        } => {
            let consts: ConsensusConsts = state.consensus_consts;
            DifficultyCalc::new(consts.pow)
        }
        Block {
            header:
                BlockHeader {
                    genesis_state: None,
                    ..
                },
            ..
        } => Err(StryiCoreError::StorageError {
            layer: StorageLayer::Block,
            err: "`genesis_state` is None in the genesis block".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_work() {
        let half = HashThreshold::ONE.shl_vartime(255);
        let quarter = HashThreshold::ONE.shl_vartime(254);
        for (target, expected) in [
            (HashThreshold::ZERO, ChainWork::ONE.shl_vartime(256)),
            (HashThreshold::ONE, ChainWork::ONE.shl_vartime(255)),
            (
                HashThreshold::from(2u64),
                ChainWork::from_be_hex(concat!(
                    "0000000000000000000000000000000000000000000000000000000000000000",
                    "5555555555555555555555555555555555555555555555555555555555555555"
                )),
            ),
            (quarter - HashThreshold::ONE, ChainWork::from(4u64)),
            (quarter, ChainWork::from(3u64)),
            (half - HashThreshold::ONE, ChainWork::from(2u64)),
            (half, ChainWork::ONE),
            (half + HashThreshold::ONE, ChainWork::ONE),
            (HashThreshold::MAX - HashThreshold::ONE, ChainWork::ONE),
            (HashThreshold::MAX, ChainWork::ONE),
        ] {
            assert_eq!(block_work(&target), expected, "target {target:?}");
        }
    }

    #[test]
    fn test_add_work() {
        let work = ChainWork::ONE.shl_vartime(255);
        assert_eq!(
            add_work(work, work).unwrap(),
            ChainWork::ONE.shl_vartime(256)
        );
        assert_eq!(
            add_work(ChainWork::MAX, ChainWork::ZERO).unwrap(),
            ChainWork::MAX
        );
        assert_eq!(
            add_work(ChainWork::MAX, ChainWork::ONE),
            Err(StryiCoreError::consensus_chain_selection(
                "chain work overflow"
            ))
        );
    }

    #[test]
    fn test_threshold_bytes() {
        let value = HashThreshold::ONE.shl_vartime(255) + HashThreshold::from(258u64);
        let mut bytes = [0; 32];
        bytes[..2].copy_from_slice(&[2, 1]);
        bytes[31] = 0x80;
        for (value, bytes) in [
            (HashThreshold::ZERO, [0; 32]),
            (value, bytes),
            (HashThreshold::MAX, [0xff; 32]),
        ] {
            assert_eq!(threshold_from_bytes(&bytes).unwrap(), value);
            assert_eq!(threshold_to_bytes(&value), bytes);
        }
        for n in [0, 1, 31, 33, 64] {
            assert_eq!(
                threshold_from_bytes(&vec![0; n]),
                Err("hash_threshold must contain exactly 32 bytes"),
                "length {n}"
            );
        }
    }

    #[test]
    fn test_chain_work_bytes() {
        let value = ChainWork::ONE.shl_vartime(511)
            + ChainWork::ONE.shl_vartime(256)
            + ChainWork::from(258u64);
        let mut bytes = [0; 64];
        bytes[..2].copy_from_slice(&[2, 1]);
        bytes[32] = 1;
        bytes[63] = 0x80;
        for (value, bytes) in [
            (ChainWork::ZERO, [0; 64]),
            (value, bytes),
            (ChainWork::MAX, [0xff; 64]),
        ] {
            assert_eq!(chain_work_from_bytes(&bytes).unwrap(), value);
            assert_eq!(chain_work_to_bytes(&value), bytes);
        }
        for n in [0, 1, 32, 63, 65] {
            assert_eq!(
                chain_work_from_bytes(&vec![0; n]),
                Err("chain_work must contain exactly 64 bytes"),
                "length {n}"
            );
        }
    }
}
