use crate::StryiCoreError;
use crate::block::{Block, BlockHeader};
use crate::consensus::ConsensusConsts;
use crate::error::StorageLayer;
use crate::storage::{BlockStorage, StorageStats};
use crypto_bigint::{CheckedAdd, NonZero};
use futures::future::{BoxFuture, ready};
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

/// Async difficulty calculator, returns required bits amount for specified height.
/// Currently just wraps pure (in a functional programming way) `difficulty_bits_for_height`,
/// but takes generic `S` so it can be replaced by any better implementation later
/// (that may consider network load, average new block arriving time, etc)
pub type DifficultyCalc<S> = Arc<
    dyn for<'a> Fn(&'a S, u64) -> BoxFuture<'a, Result<u8, StryiCoreError>> + Send + Sync + 'static,
>;

/// Reads consensus rules from genesis and returns a [`DifficultyCalc`].
pub async fn load_rules_and_build_calc<DB>(
    db: Arc<RwLock<DB>>,
) -> Result<DifficultyCalc<DB>, StryiCoreError>
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
            Ok(difficulty_calculator_from_consts::<DB>(consts))
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

/// Height-only difficulty calculator from the given constants (ignores db state).
#[inline]
pub fn difficulty_calculator_from_consts<S>(consts: ConsensusConsts) -> DifficultyCalc<S>
where
    S: Send + Sync + 'static,
{
    Arc::new(move |_state: &S, height| {
        let bits = consts.difficulty_bits_for_height(height);
        Box::pin(ready::<Result<u8, StryiCoreError>>(Ok(bits)))
    })
}

#[cfg(test)]
mod difficulty_calc_tests {
    use super::*;
    use crate::block::BlockHash;
    use crate::storage::StryiInMemoryStorage;
    use crate::{
        address::AccountAddress,
        block::{Block, BlockData, BlockHeader, GenesisState},
        storage::BlockStorage,
        transactions::{Transaction, TransactionData, TransactionKind, TransactionOut},
    };

    // Minimal deterministic genesis builder for this test only.
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
                // For genesis, difficulty_bits is conventionally 0; the validator should permit it.
                difficulty_bits: 0,
                timestamp: 1_700_000_000,
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
    async fn load_rules_and_build_returns_height_linear_calc() {
        // Arrange: in-memory DB with a valid genesis carrying default ConsensusConsts.
        let db = StryiInMemoryStorage::new(make_genesis_for_calc());
        let db = Arc::new(RwLock::new(db));

        // Act: build difficulty calculator from genesis rules/consts.
        let calc = load_rules_and_build_calc(db.clone())
            .await
            .expect("calculator");

        // We know the genesis carried ConsensusConsts::default(); verify that the
        // calculator follows `difficulty_bits_for_height` for several check points.
        let consts = ConsensusConsts::default();

        // Helper to evaluate the calc at a given height using a &DB snapshot.
        async fn eval_at<DB>(calc: &DifficultyCalc<DB>, db: &Arc<RwLock<DB>>, h: u64) -> u8
        where
            DB: BlockStorage + StorageStats + Send + Sync + 'static,
        {
            let guard = db.read().await; // &DB for the calc (it ignores state in our builder)
            (calc)(&*guard, h).await.expect("calc result")
        }

        // Assert: a few representative heights around the default interval boundary.
        // The default interval is 100 (see ConsensusConsts::default).
        let h0 = 0;
        let h1 = 1;
        let h100 = 100;
        let h101 = 101;
        let h250 = 250;

        let got0 = eval_at(&calc, &db, h0).await;
        let got1 = eval_at(&calc, &db, h1).await;
        let got100 = eval_at(&calc, &db, h100).await;
        let got101 = eval_at(&calc, &db, h101).await;
        let got250 = eval_at(&calc, &db, h250).await;

        assert_eq!(got0, consts.difficulty_bits_for_height(h0), "height {}", h0);
        assert_eq!(got1, consts.difficulty_bits_for_height(h1), "height {}", h1);
        assert_eq!(
            got100,
            consts.difficulty_bits_for_height(h100),
            "height {}",
            h100
        );
        assert_eq!(
            got101,
            consts.difficulty_bits_for_height(h101),
            "height {}",
            h101
        );
        assert_eq!(
            got250,
            consts.difficulty_bits_for_height(h250),
            "height {}",
            h250
        );
    }
}
