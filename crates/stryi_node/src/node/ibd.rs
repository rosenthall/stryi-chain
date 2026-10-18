use crate::error::StryiNodeError;
use std::time::Instant;
use stryi_core::StryiCoreError;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::{ConsensusEngine, ConsensusVerdict};
use tracing::{debug, error, info, trace, warn};

/// IBD helper feeds blocks to the consensus engine in-order
/// and logs everything (batch start/end, per-block details, verdicts, and summary).
/// Immediately exists if any of the blocks is `Rejected`
#[tracing::instrument(level = "info", skip(engine, blocks_in_order))]
pub fn ingest_ibd_batch<E>(
    engine: &mut E,
    mut blocks_in_order: Vec<Block>,
) -> Result<(), StryiNodeError>
where
    E: ConsensusEngine<Error = StryiCoreError>,
{
    let started_at = Instant::now();
    let batch_len = blocks_in_order.len();

    // Verdict counters
    let mut cnt_applied: u64 = 0;
    let mut cnt_buffered: u64 = 0;
    let mut cnt_already_chain: u64 = 0;
    let mut cnt_already_fork: u64 = 0;
    let mut cnt_reorgs: u64 = 0;

    info!("IBD batch start: count={}", batch_len);

    // Sequentially feed blocks to the engine (IBD must provide ordered batches)
    for (i, b) in blocks_in_order.drain(..).enumerate() {
        // Collect diagnostic context before on_block so we can always log it
        let height: u64 = b.header.height;
        let prev: BlockHash = b.header.previous_block_hash;
        let threshold = b.header.hash_threshold;
        let is_genesis: bool = b.header.is_genesis();
        let tx_count: usize = b.data.transactions.len();
        let hash: BlockHash = b.block_hash();

        debug!(
            "IBD[{}/{}] on_block -> height={}, hash={}, prev={}, threshold={}, is_genesis={}, txs={}",
            i + 1,
            batch_len,
            height,
            hash,
            prev,
            threshold,
            is_genesis,
            tx_count
        );

        match engine.on_block(b)? {
            ConsensusVerdict::Applied { transition } => {
                cnt_applied += 1;
                info!(
                    "APPLIED: height={}, hash={}, chain_work={}",
                    height, hash, transition.chain_work
                );
            }

            ConsensusVerdict::AlreadyIncludedInChain => {
                cnt_already_chain += 1;
                trace!("ALREADY_IN_CHAIN: height={}, hash={}", height, hash);
            }

            ConsensusVerdict::AlreadyKnownInForkTree => {
                cnt_already_fork += 1;
                trace!("ALREADY_IN_FORK_TREE: height={}, hash={}", height, hash);
            }

            ConsensusVerdict::CausedReorganization { transition } => {
                cnt_reorgs += 1;
                warn!(
                    "REORG: new_tip height={}, hash={}; removed_blocks_count={}",
                    transition.new_head.0,
                    transition.new_head.1,
                    transition.detached.len()
                );

                // A detailed list goes to TRACE to avoid noisy WARNs
                for (height, hash) in transition.detached {
                    trace!("REORG_REMOVED: height={}, old_hash={}", height, hash);
                }
            }

            ConsensusVerdict::Buffered => {
                cnt_buffered += 1;
                trace!("BUFFERED: height={}, hash={}", height, hash);
            }

            ConsensusVerdict::Rejected(e) => {
                error!(
                    "REJECTED: height={}, hash={}, error={}",
                    height,
                    hash,
                    e.to_string()
                );

                return Err(StryiNodeError::other(format!(
                    "IBD rejected block {} at height {}: {}",
                    hash, height, e
                )));
            }
        }
    }

    let elapsed = started_at.elapsed();
    info!(
        "IBD batch done: total={}, applied={}, buffered={}, already_in_chain={}, already_in_fork_tree={}, reorgs={}, elapsed_ms={}",
        batch_len,
        cnt_applied,
        cnt_buffered,
        cnt_already_chain,
        cnt_already_fork,
        cnt_reorgs,
        elapsed.as_millis()
    );

    Ok(())
}
