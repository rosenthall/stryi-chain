use crate::error::StryiNodeError;
use crate::grpc_services::blockchain_sync_client::BlockchainSyncClient;
use crate::grpc_services::{Block as PbBlock, BlockHeightRange};
use crate::grpc_services::{BlockHashList, ChainInfo as PbChainInfo};
use futures_util::StreamExt;
use stryi_core::block::{Block, BlockHash};
use stryi_core::difficulty::{ChainWork, chain_work_from_bytes};
use stryi_network::PeerId;
use tonic::transport::Channel;

/// A validated, typed view of a peer's on-wire [`PbChainInfo`].
///
/// The wire message carries raw protobuf types; decoding happens exactly once
/// here so sync code compares typed [`ChainWork`] instead of re-parsing or
/// truncating raw numbers at every use site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteChainInfo {
    pub height: u64,
    pub tip_hash: BlockHash,
    pub chain_work: ChainWork,
    pub last_update_time: u64,
    pub protocol_version: u32,
    pub chain_name: String,
}

impl TryFrom<PbChainInfo> for RemoteChainInfo {
    type Error = StryiNodeError;

    fn try_from(pb: PbChainInfo) -> Result<Self, Self::Error> {
        let tip_hash = BlockHash::from_hash_string(&pb.latest_block_hash)
            .map_err(|e| StryiNodeError::other(format!("invalid remote tip hash: {e}")))?;

        let chain_work = chain_work_from_bytes(&pb.chain_work)
            .map_err(|e| StryiNodeError::other(format!("invalid remote chain_work: {e}")))?;

        Ok(RemoteChainInfo {
            height: pb.height,
            tip_hash,
            chain_work,
            last_update_time: pb.last_update_time,
            protocol_version: pb.protocol_version,
            chain_name: pb.chain_name,
        })
    }
}

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
        self.peer_id
    }

    pub async fn request_chain_info(&mut self) -> Result<RemoteChainInfo, StryiNodeError> {
        let response = self
            .grpc_client
            .get_chain_info(tonic::Request::new(()))
            .await
            .map_err(|e| {
                StryiNodeError::other(format!("Failed to get chain info from gRPC service: {e}"))
            })?;

        RemoteChainInfo::try_from(response.into_inner())
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
