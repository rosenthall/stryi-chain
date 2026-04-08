//! LCA (Last Common Ancestor) detection between local and remote chains.

use crate::error::StryiNodeError;
use crate::node::remote_peer::RemotePeer;
use std::sync::Arc;
use stryi_core::block::BlockHash;
use stryi_core::storage::BlockStorage;
use stryi_storage::StryiStorage;
use tokio::sync::RwLock;
use tracing::debug;

/// Finds the highest block height shared by the local chain and a remote peer.
pub(crate) async fn find_lca(
    storage: &Arc<RwLock<StryiStorage>>,
    remote_peer: &mut RemotePeer,
    remote_tip_height: u64,
    local_tip_height: u64,
) -> Result<(u64, BlockHash), StryiNodeError> {
    let mut low_height: u64 = 0;
    let mut high_height: u64 = std::cmp::min(remote_tip_height, local_tip_height);

    let mut best_height: u64 = 0;
    let mut best_hash: BlockHash = BlockHash::empty();

    debug!(
        remote_tip_height,
        local_tip_height,
        search_high = high_height,
        "Starting LCA binary search"
    );

    while low_height <= high_height {
        let mid_height = low_height + ((high_height - low_height) / 2);

        debug!(
            low_height,
            high_height, mid_height, best_height, "LCA binary search iteration"
        );

        let local_block = {
            let s = storage.read().await;
            s.get_block_by_height(mid_height)
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

        let remote_block = remote_peer.request_block_by_height(mid_height).await?;

        let local_hash = local_block.block_hash();
        let remote_hash = remote_block.block_hash();

        debug!(
            mid_height,
            ?local_hash,
            ?remote_hash,
            hashes_match = (local_hash == remote_hash),
            "Compared local and remote block hashes at mid height"
        );

        if local_hash == remote_hash {
            best_height = mid_height;
            best_hash = local_hash;
            low_height = mid_height.saturating_add(1);
        } else {
            if mid_height == 0 {
                debug!("Hash mismatch at genesis height, stopping LCA search");
                break;
            }

            high_height = mid_height - 1;
        }
    }

    debug!(best_height, ?best_hash, "Finished LCA binary search");

    Ok((best_height, best_hash))
}
