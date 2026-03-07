use crate::error::StryiNodeError;
use crate::grpc_services::blockchain_sync_client::BlockchainSyncClient;
use crate::grpc_services::{Block as PbBlock, BlockHeightRange};
use crate::grpc_services::{BlockHashList, ChainInfo};
use futures_util::StreamExt;
use stryi_core::block::{Block, BlockHash};
use stryi_network::PeerId;
use tonic::transport::Channel;

// An abstraction that represents a remote peer and provides comfy
// methods to interact with it
pub struct RemotePeer {
    peer_id: PeerId,
    grpc_client: BlockchainSyncClient<Channel>,
}

impl RemotePeer {
    pub(crate) fn new(peer_id: PeerId, grpc_client: BlockchainSyncClient<Channel>) -> Self {
        Self {
            peer_id,
            grpc_client,
        }
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id.clone()
    }

    pub async fn request_chain_info(&mut self) -> Result<ChainInfo, StryiNodeError> {
        self.grpc_client
            .get_chain_info(tonic::Request::new(()))
            .await
            .map(|response| {
                let chain_info = response.into_inner();
                Ok(chain_info)
            })
            .map_err(|e| {
                StryiNodeError::other(format!("Failed to get chain info from gRPC service: {e}"))
            })?
    }

    /// Simple helper requests the genesis block of a peer.
    pub async fn request_genesis(&mut self) -> Result<Block, StryiNodeError> {
        self.request_block_by_hash(BlockHash::empty()).await
    }

    /// Get a block of a peer by its hash.
    pub async fn request_block_by_hash(
        &mut self,
        hash: BlockHash,
    ) -> Result<Block, StryiNodeError> {
        self.request_blocks_by_hashes(vec![hash], 1)
            .await?
            .pop()
            .ok_or(StryiNodeError::other("block not found"))
    }

    /// Get blocks by a list of hashes and returns a vector.
    /// Useful for LCA helpers or ad-hoc fetches.
    pub async fn request_blocks_by_hashes(
        &mut self,
        hashes: Vec<BlockHash>,
        max_blocks: usize,
    ) -> Result<Vec<Block>, StryiNodeError> {
        if hashes.len() > max_blocks {
            return Err(StryiNodeError::other("too many hashes requested"));
        }

        let resp = self
            .grpc_client
            .get_blocks_by_hash(BlockHashList {
                block_hashes: hashes.into_iter().map(|h| h.to_string()).collect(),
            })
            .await
            .map_err(|e| StryiNodeError::other(format!("get_blocks_by_hash failed: {e}")))?;

        let mut stream = resp.into_inner();
        let mut out = Vec::new();

        while let Some(item) = stream.next().await {
            let pb: PbBlock = item.map_err(|status| {
                StryiNodeError::other(format!("get_blocks_by_hash stream error: {status}"))
            })?;

            let block = pb.try_into().map_err(|e| {
                StryiNodeError::other(format!("failed to convert wire Block: {e:?}"))
            })?;

            out.push(block);
            if out.len() >= max_blocks {
                break;
            }
        }

        Ok(out)
    }

    /// Get blocks by a height range and returns a vector.
    /// Useful for LCA helpers or ad-hoc fetches.
    /// Respects `max_blocks`
    pub async fn request_blocks_by_height_range(
        &mut self,
        start_height: u64,
        end_height: u64,
        max_blocks: u32,
    ) -> Result<Vec<Block>, StryiNodeError> {
        let resp = self
            .grpc_client
            .get_blocks_by_height(BlockHeightRange {
                start_height,
                end_height,
                max_blocks,
            })
            .await
            .map_err(|e| {
                StryiNodeError::other(format!(
                    "get_blocks_by_height({start_height}..={end_height}) failed: {e}"
                ))
            })?;

        let mut stream = resp.into_inner();
        let mut out = Vec::new();

        while let Some(item) = stream.next().await {
            let pb: PbBlock = item.map_err(|status| {
                StryiNodeError::other(format!("get_blocks_by_height stream error: {status}"))
            })?;

            let block: Block = pb.try_into().map_err(|e| {
                StryiNodeError::other(format!("failed to convert wire Block: {e:?}"))
            })?;

            out.push(block);

            if out.len() as u32 >= max_blocks {
                break;
            }
        }

        Ok(out)
    }

    /// A helper to request a block of a peer by its height.
    pub async fn request_block_by_height(&mut self, height: u64) -> Result<Block, StryiNodeError> {
        self.request_blocks_by_height_range(height, height, 1)
            .await?
            .pop()
            .ok_or(StryiNodeError::other("block not found"))
    }
}
