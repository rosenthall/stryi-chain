use crate::error::StryiNodeError;
use crate::grpc_services::BlockHeightRange;
use crate::grpc_services::blockchain_sync_client::BlockchainSyncClient;
use futures_util::StreamExt;
use std::time::Instant;
use stryi_core::StryiCoreError;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::{ConsensusEngine, ConsensusVerdict};
use tonic::transport::Channel;
use tracing::{debug, error, info, trace, warn};

// Fetch a contiguous batch [start_height ..= end_height] (subject to server-side cap).
pub(crate) async fn fetch_blocks_batch(
    grpc: &mut BlockchainSyncClient<Channel>,
    batch_size: usize,
    start_height: u64,
    end_height: u64,
) -> Result<Vec<Block>, StryiNodeError> {
    let resp = grpc
        .get_blocks_by_height(BlockHeightRange {
            start_height,
            end_height,
            max_blocks: batch_size as u32,
        })
        .await
        .map_err(|e| {
            StryiNodeError::other(format!(
                "get_blocks_by_height({start_height}..={end_height}) failed: {e}"
            ))
        })?;

    let mut stream = resp.into_inner();
    let mut batch: Vec<Block> = Vec::new();

    while let Some(item) = stream.next().await {
        let pb = item.map_err(|e| StryiNodeError::other(format!("stream error: {e}")))?;
        let block: Block = pb
            .try_into()
            .map_err(|e| StryiNodeError::other(format!("failed to convert wire Block: {e:?}")))?;
        batch.push(block);
    }

    Ok(batch)
}

/// IBD helper, feeds blocks to the consensus engine in-order
/// and logs everything (batch start/end, per-block details, verdicts, and summary).
/// Immediately exists if any of blocks is `Rejected`
#[tracing::instrument(level = "info", skip(engine, blocks_in_order))]
pub async fn ingest_ibd_batch<E>(
    engine: &mut E,
    mut blocks_in_order: Vec<Block>,
) -> Result<(), StryiCoreError>
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
        let bits: u8 = b.header.difficulty_bits;
        let is_genesis: bool = b.header.is_genesis();
        let tx_count: usize = b.data.transactions.len();
        let hash: BlockHash = b.block_hash();

        debug!(
            "IBD[{}/{}] on_block -> height={}, hash={}, prev={}, bits={}, is_genesis={}, txs={}",
            i + 1,
            batch_len,
            height,
            hash,
            prev,
            bits,
            is_genesis,
            tx_count
        );

        match engine.on_block(b).await? {
            ConsensusVerdict::Applied {
                new_chain_complexity,
            } => {
                cnt_applied += 1;
                info!(
                    "APPLIED: height={}, hash={}, new_chain_complexity={}",
                    height, hash, new_chain_complexity
                );
            }

            ConsensusVerdict::BufferedIntoForkTree {
                common_ancestor_height: (ancestor_hash, ancestor_h),
            } => {
                cnt_buffered += 1;
                warn!(
                    "BUFFERED (fork): height={}, hash={}, parent_not_tip; common_ancestor=({}, height={})",
                    height, hash, ancestor_hash, ancestor_h
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

            ConsensusVerdict::CausedReorganization { mut deleted_blocks } => {
                cnt_reorgs += 1;
                let removed_count = deleted_blocks.len();
                warn!(
                    "REORG: new_tip height={}, hash={}; removed_blocks_count={}",
                    height, hash, removed_count
                );
                // Detailed list goes to TRACE to avoid noisy WARNs
                for (h, del_hash) in deleted_blocks.drain() {
                    trace!("REORG_REMOVED: height={}, old_hash={}", h, del_hash);
                }
            }

            ConsensusVerdict::Rejected(e) => {
                error!(
                    "REJECTED: height={}, hash={}, error={}",
                    height,
                    hash,
                    e.to_string()
                );
                return Err(e);
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
