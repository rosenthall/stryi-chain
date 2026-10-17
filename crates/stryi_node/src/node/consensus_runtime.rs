use crate::error::StryiNodeError;
use std::sync::Arc;
use stryi_core::StryiCoreError;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::{
    CommittedTransition, ConsensusEngine, ConsensusVerdict, StryiConsensusEngine,
};
use stryi_core::difficulty::ChainWork;
use stryi_core::mempool::MemPool;
use stryi_storage::StryiStorage;
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio_util::sync::CancellationToken;

/// Runs synchronous consensus validation/commits on a blocking worker
/// and updates mempool + tip subscribers afterwards.
#[derive(Clone)]
pub(crate) struct ConsensusRuntime {
    pub engine: Arc<Mutex<StryiConsensusEngine<StryiStorage>>>,
    pub storage: StryiStorage,
    pub mempool: Arc<RwLock<MemPool>>,
    pub tip_updates: broadcast::Sender<BlockHash>,
    pub cancel: CancellationToken,
}

#[derive(Debug)]
pub(crate) enum ConsensusOutcome {
    Applied {
        new_head: (u64, BlockHash),
        chain_work: ChainWork,
    },
    CausedReorganization {
        new_head: (u64, BlockHash),
        chain_work: ChainWork,
        attached: usize,
        detached: usize,
    },
    AlreadyIncludedInChain,
    AlreadyKnownInForkTree,
    Buffered,
    Rejected(StryiCoreError),
}

impl ConsensusRuntime {
    /// Applies a single block (returns `Ok(None)` if node is cancelled before worker starts it).
    pub async fn apply_block(
        &self,
        block: Block,
    ) -> Result<Option<ConsensusOutcome>, StryiNodeError> {
        Ok(self.apply_blocks(vec![block]).await?.into_iter().next())
    }

    /// Processes blocks sequentially on a blocking thread, stopping early on rejection or cancellation.
    /// Once a block starts, dropping the caller future won't interrupt its DB commit or mempool update.
    pub async fn apply_blocks(
        &self,
        blocks: Vec<Block>,
    ) -> Result<Vec<ConsensusOutcome>, StryiNodeError> {
        if self.cancel.is_cancelled() {
            return Err(StryiNodeError::other("node is stopping"));
        }

        // Lock engine before spawning so waiting callers don't tie up blocking threads
        let mut engine = self.engine.clone().lock_owned().await;

        if self.cancel.is_cancelled() {
            return Err(StryiNodeError::other("node is stopping"));
        }

        let runtime = self.clone();

        tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.apply_block_batch(&mut engine, blocks)
            }));

            match result {
                Ok(Ok(verdicts)) => Ok(verdicts),
                Ok(Err(error)) => {
                    if error.is_fatal() {
                        runtime.storage.mark_failed();
                    }

                    Err(StryiNodeError::from(error))
                }
                Err(_) => {
                    runtime.storage.mark_failed();

                    Err(StryiNodeError::other("consensus worker panicked"))
                }
            }
        })
        .await
        .map_err(|e| StryiNodeError::other(format!("consensus worker failed: {e}")))?
    }

    fn apply_block_batch(
        &self,
        engine: &mut StryiConsensusEngine<StryiStorage>,
        blocks: Vec<Block>,
    ) -> Result<Vec<ConsensusOutcome>, StryiCoreError> {
        let mut verdicts = Vec::with_capacity(blocks.len());

        for block in blocks {
            if self.cancel.is_cancelled() {
                break;
            }

            let outcome = match engine.on_block(block)? {
                ConsensusVerdict::Applied { transition } => {
                    let outcome = ConsensusOutcome::Applied {
                        new_head: transition.new_head,
                        chain_work: transition.chain_work,
                    };
                    self.publish_transition(transition);

                    outcome
                }
                ConsensusVerdict::CausedReorganization { transition } => {
                    let outcome = ConsensusOutcome::CausedReorganization {
                        new_head: transition.new_head,
                        chain_work: transition.chain_work,
                        attached: transition.attached.len(),
                        detached: transition.detached.len(),
                    };
                    self.publish_transition(transition);

                    outcome
                }
                ConsensusVerdict::AlreadyIncludedInChain => {
                    ConsensusOutcome::AlreadyIncludedInChain
                }
                ConsensusVerdict::AlreadyKnownInForkTree => {
                    ConsensusOutcome::AlreadyKnownInForkTree
                }
                ConsensusVerdict::Buffered => ConsensusOutcome::Buffered,
                ConsensusVerdict::Rejected(error) => ConsensusOutcome::Rejected(error),
            };

            let rejected = matches!(outcome, ConsensusOutcome::Rejected(_));
            verdicts.push(outcome);

            if rejected {
                break;
            }
        }

        Ok(verdicts)
    }

    fn publish_transition(&self, transition: CommittedTransition) {
        let mut pool = self.mempool.blocking_write();

        for attached in transition.attached {
            pool.update_on_block(attached.data);
        }

        drop(pool);

        let _ = self.tip_updates.send(transition.new_head.1);
    }
}
