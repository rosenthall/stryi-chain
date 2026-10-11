use crate::StryiCoreError;
use crate::block::{Block, BlockHash, BlockHeader};
use crate::consensus::ConsensusConsts;
use crate::error::StorageLayer;
use crate::storage::{BlockStorage, StorageStats};
use crypto_bigint::{CheckedAdd, NonZero};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

mod asert;
pub use asert::asert_upper;

pub use crypto_bigint::U256 as HashThreshold;
pub use crypto_bigint::U512 as ChainWork;

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

#[cfg(test)]
mod pow_number_tests {
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
/// It returns the configured `initial_hash_threshold` for every normal
/// candidate block.
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

    /// Returns the hash threshold required for a candidate block whose parent is `parent_hash`.
    ///
    /// Missing parent/storage errors are errors, never defaults. ZERO applies only
    /// to the genesis header and is handled by validation, never to a child of genesis.
    pub async fn next_hash_threshold<S>(
        &self,
        state: &S,
        parent_hash: BlockHash,
    ) -> Result<HashThreshold, StryiCoreError>
    where
        S: BlockStorage + Send + Sync,
    {
        let parent = state.get_block_by_hash(parent_hash).await.map_err(|e| {
            StryiCoreError::StorageError {
                layer: StorageLayer::Block,
                err: format!(
                    "failed to read parent {} while computing the next hash threshold: {}",
                    parent_hash, e
                ),
            }
        })?;

        if parent.is_none() {
            return Err(StryiCoreError::ConsensusDifficultyAdjustmentFailed {
                details: format!(
                    "parent block {} not found while computing the next hash threshold",
                    parent_hash
                ),
            });
        }

        Ok(self.params.initial_hash_threshold)
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
mod difficulty_calc_tests {
    use super::*;
    use crate::address::AccountAddress;
    use crate::block::{BlockData, GenesisState};
    use crate::storage::StryiInMemoryStorage;
    use crate::transactions::{Transaction, TransactionData, TransactionKind, TransactionOut};

    // Minimal deterministic genesis builder for these tests only.
    fn make_genesis_for_calc() -> Block {
        let premine_owner = AccountAddress::new(&[0u8; 20]);
        let genesis_tx = Transaction {
            data: TransactionData {
                version: 1,
                kind: TransactionKind::Genesis,
                inputs: vec![],
                outputs: vec![TransactionOut {
                    value: 1_000_000,
                    recipient: premine_owner,
                }],
            },
            signature: Default::default(),
        };

        Block {
            header: BlockHeader {
                version: 1,
                merkle_root_hash: Block::compute_merkle_root(std::slice::from_ref(&genesis_tx)),
                previous_block_hash: BlockHash::empty(),
                height: 0,
                // Genesis carries the zero hash threshold; genesis work is zero.
                hash_threshold: HashThreshold::ZERO,
                timestamp: 0,
                nonce: 0,

                // Critical: genesis_state must be Some to carry ConsensusConsts from genesis.
                genesis_state: Some(GenesisState::default()),
            },
            data: BlockData {
                transactions: vec![genesis_tx],
            },
        }
    }

    #[tokio::test]
    async fn pow_v2_load_rules_builds_calc_from_genesis() {
        // Arrange: in-memory DB with a valid genesis carrying default ConsensusConsts.
        let db = StryiInMemoryStorage::new(make_genesis_for_calc());
        let db = Arc::new(RwLock::new(db));

        // Act: build difficulty calculator from genesis rules/consts.
        let calc = load_rules_and_build_calc(db.clone())
            .await
            .expect("calculator");

        // Assert: the calculator follows the genesis PowParams; the first normal
        // candidate (child of genesis) gets the initial threshold, not ZERO.
        let guard = db.read().await;
        let child_of_genesis = calc
            .next_hash_threshold(&*guard, BlockHash::empty())
            .await
            .expect("genesis parent exists");
        assert_eq!(
            child_of_genesis,
            ConsensusConsts::default().pow.initial_hash_threshold
        );
        assert_ne!(child_of_genesis, HashThreshold::ZERO);
    }
}
