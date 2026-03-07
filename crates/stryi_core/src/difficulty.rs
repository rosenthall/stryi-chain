use crate::StryiCoreError;
use crate::block::{Block, BlockHeader};
use crate::consensus::ConsensusConsts;
use crate::error::StorageLayer;
use crate::storage::{BlockStorage, StorageStats};
use futures::future::{BoxFuture, ready};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Asynchronous difficulty calculator injected as a function.
/// **Currently, a thin wrapper over `difficulty_bits_for_height`, but extensible to state-dependent policies.**
///
// - `&S` is the chain state, may include any required data to calculate difficulty; `usize` is the block height.
// - HRTB (`for<'a>`) ties the future’s lifetime to the borrow of `&S`.
// - Returns a `u8` difficulty or `StryiCoreError`.
// - `Send + Sync + 'static` enables sharing across threads.
// - `BoxFuture` erases the concrete future type.
pub type DifficultyCalc<S> = Arc<
    dyn for<'a> Fn(&'a S, u64) -> BoxFuture<'a, Result<u8, StryiCoreError>> + Send + Sync + 'static,
>;

/// Builds a difficulty calculator from consensus rules stored in the genesis block.
/// Currently, height-only, but extensible to state-dependent policies.
pub async fn load_rules_and_build_calc<DB>(
    db: Arc<RwLock<DB>>,
) -> Result<DifficultyCalc<DB>, StryiCoreError>
where
    DB: BlockStorage + StorageStats + Send + Sync + 'static,
{
    // Try to read the genesis block from storage (height = 0).
    let genesis_opt = {
        // Short, read-only guard scope.
        let guard = db.read().await;
        // Fetch the block at height 0.
        guard.get_block_by_height(0).await
    }
    .map_err(|e| StryiCoreError::StorageError {
        layer: StorageLayer::Block,
        err: format!(
            "failed to read genesis while constructing DifficultyCalc: {}",
            e
        ),
    })?;

    // Fail fast if the genesis block is missing.
    let genesis_block = match genesis_opt {
        Some(b) => b,
        None => {
            return Err(StryiCoreError::StorageError {
                layer: StorageLayer::Block,
                err: "genesis block not found (height 0)".to_string(),
            });
        }
    };

    // Destructure and ensure that `genesis_state` is present.
    match genesis_block {
        Block {
            header:
                BlockHeader {
                    genesis_state: Some(state),
                    ..
                },
            ..
        } => {
            // Extract immutable consensus constants from the genesis state.
            let consts: ConsensusConsts = state.consensus_consts;
            // Build a height-only difficulty calculator that captures only `consts`.
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

/// Builds a difficulty calculator from immutable consensus constants.
/// This particular calculator implementation only captures `ConsensusConsts` and does not rely on db state.
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
