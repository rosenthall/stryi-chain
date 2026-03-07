//! LCA (Last Common Ancestor) detection between local and remote chains.

use crate::error::StryiNodeError;
use crate::node::ConnectedNode;
use crate::node::remote_peer::RemotePeer;
use stryi_core::block::BlockHash;
use stryi_core::storage::BlockStorage;

impl ConnectedNode {
    /// Binary-search for the highest common block between us and a remote peer.
    pub(crate) async fn find_last_common_ancestor(
        &self,
        remote_peer: &mut RemotePeer,
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
            // calculate midpoint
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
            let remote_block = remote_peer.request_block_by_height(mid_height).await?;

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
