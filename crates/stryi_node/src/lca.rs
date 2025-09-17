//! LCA - last common ancestor.
//! This module contains some helpers for findings LCAs in two chains:
//! regularly, between local chain and external peers' chains.

use crate::error::StryiNodeError;
use crate::grpc_services::blockchain_sync_client::BlockchainSyncClient;
use crate::grpc_services::{BlockHashList, BlockHeightRange};
use crate::node::StryiChainNode;
use futures_util::StreamExt;
use stryi_core::block::{Block, BlockHash};
use stryi_core::storage::BlockStorage;
use tonic::transport::Channel;

impl StryiChainNode {
    /// checks if peer has a given block hash.
    pub async fn has_remote_block_by_hash(
        grpc: &mut BlockchainSyncClient<Channel>,
        hash: BlockHash,
    ) -> Result<bool, StryiNodeError> {
        let resp = grpc
            .get_blocks_by_hash(BlockHashList {
                block_hashes: vec![hash.to_string()],
            })
            .await
            .map_err(|e| StryiNodeError::other(format!("get_blocks_by_hash failed: {e}")))?;
        let mut stream = resp.into_inner();

        match stream.next().await {
            Some(Ok(_)) => Ok(true),
            Some(Err(status)) if status.code() == tonic::Code::NotFound => Ok(false),
            Some(Err(status)) => Err(StryiNodeError::other(format!(
                "get_blocks_by_hash stream error: {status}"
            ))),
            None => Ok(false),
        }
    }

    /// Fetch exactly one remote block by the given height.
    async fn fetch_remote_block_at(
        &self,
        grpc: &mut BlockchainSyncClient<Channel>,
        height: u64,
    ) -> Result<Block, StryiNodeError> {
        let resp = grpc
            .get_blocks_by_height(BlockHeightRange {
                start_height: height,
                end_height: height,
                max_blocks: 100,
            })
            .await
            .map_err(|e| {
                StryiNodeError::other(format!("get_blocks_by_height({height}) failed: {e}"))
            })?;
        let mut stream = resp.into_inner();

        let item = stream
            .next()
            .await
            .ok_or_else(|| StryiNodeError::other(format!("no block returned at height {height}")))?
            .map_err(|e| StryiNodeError::other(format!("stream error at height {height}: {e}")))?;

        let block: Block = item.try_into().map_err(|e| {
            StryiNodeError::other(format!("failed to convert wire Block @h={height}: {e:?}"))
        })?;
        Ok(block)
    }

    /// Binary-search the highest height where local and remote blocks are equal.
    /// Precondition: genesis is already validated and identical on both sides.
    pub(crate) async fn find_last_common_ancestor(
        &self,
        grpc: &mut BlockchainSyncClient<Channel>,
        remote_tip_height: u64,
        local_tip_height: u64,
    ) -> Result<(u64, BlockHash), StryiNodeError> {
        // Search range [low_height .. high_height]
        let mut low_height: u64 = 0;
        let mut high_height: u64 = std::cmp::min(remote_tip_height, local_tip_height);

        // Best known common point (defaults to genesis)
        let mut best_height: u64 = 0;
        let mut best_hash: BlockHash = BlockHash::empty();

        while low_height <= high_height {
            // Midpoint (overflow-safe)
            let mid_height = low_height + ((high_height - low_height) / 2);

            // Local block at mid_height must exist (mid <= local_tip_height)
            let local_block = {
                let storage = self.storage.read().await;
                storage
                    .get_block_by_height(mid_height)
                    .await
                    .map_err(|e| {
                        StryiNodeError::other(format!(
                            "local get_block_by_height({mid_height}) failed: {e}"
                        ))
                    })?
                    .ok_or_else(|| {
                        StryiNodeError::other(format!("local block missing at height {mid_height}"))
                    })?
            };

            // Remote block at the same height
            let remote_block = self.fetch_remote_block_at(grpc, mid_height).await?;

            let local_hash = local_block.block_hash();
            let remote_hash = remote_block.block_hash();

            if local_hash == remote_hash {
                // mid is a common ancestor; try to move higher
                best_height = mid_height;
                best_hash = local_hash;
                low_height = mid_height.saturating_add(1);
            } else {
                // diverged at or below mid; search lower half
                if mid_height == 0 {
                    break;
                }
                high_height = mid_height - 1;
            }
        }

        Ok((best_height, best_hash))
    }
}
