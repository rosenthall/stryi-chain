use crate::grpc_services::blockchain_sync_server::BlockchainSyncServer;
use crate::grpc_services::{
    // Some aliases to avoid overlapping with similar structs from stryi_core
    Block as PbBlock,
    BlockHashList,
    BlockHeader as PbBlockHeader,
    BlockHeightRange,
    ChainInfo as PbChainInfo,
    SerializedBlockBody,
};
use crate::middleware::ready::NotReadyResponder;
use futures_util::stream;
use http::{Response as HttpResponse, StatusCode};
use std::net::SocketAddr;
use std::pin::Pin;
use stryi_core::StryiCoreError;
use stryi_core::block::{Block, BlockHash, BlockHeader, GenesisState};
use stryi_core::difficulty::{chain_work_to_bytes, threshold_from_bytes, threshold_to_bytes};
use stryi_core::merkletree::MerkleHash;
use stryi_core::storage::{BlockStorage, StorageReader, StorageStats};
use tokio_stream::StreamExt;
use tonic::body::Body;
use tonic::codegen::tokio_stream::Stream;
use tonic::{Request, Response, Status};

/// Fixed value for the gRPC service name to register in the network.
pub const GRPC_SERVICE_TAG: &str = "grpc-sync";

/// Implementation of grpc sync protocol, see proto/sync.proto
#[derive(Clone)]
pub struct StryiSyncService<DB>
where
    DB: StorageReader,
{
    pub(crate) config: StryiSyncServiceConfig,
    pub(crate) storage: DB,
}

#[derive(Clone, Debug)]
pub struct StryiSyncServiceConfig {
    pub(crate) address: SocketAddr,

    /// Name of this exact chain and network, e.g `testnet`, `stryichain`, whatever
    pub(crate) chain_name: String,

    /// Numerical value that represents version of sync protocol
    pub(crate) protocol_version: usize,

    /// Value to avoid asking for entire chain quickly.
    pub(crate) max_blocks_range_per_request: usize,
}

// Implement `UnreadyServiceResponder` so sync service will properly answer even if not ready
impl<T> NotReadyResponder for BlockchainSyncServer<T>
where
    T: Send + Sync + 'static,
{
    type NotReadyResponse = HttpResponse<Body>;

    fn not_ready(&self) -> Self::NotReadyResponse {
        // Construct a gRPC "UNAVAILABLE" error response
        tonic::codegen::http::Response::builder()
            .status(StatusCode::OK) // gRPC specs typically use 200 OK here, and rely on the grpc-status header
            .header("content-type", "application/grpc")
            .header("grpc-status", "14") // 14 is "UNAVAILABLE" per https://github.com/grpc/grpc/blob/master/doc/statuscodes.md
            .header(
                "grpc-message",
                "Node is not synchronized yet. Try again later",
            )
            .body(Body::empty())
            .unwrap()
    }
}

#[tonic::async_trait]
impl<DB> crate::grpc_services::blockchain_sync_server::BlockchainSync for StryiSyncService<DB>
where
    DB: StorageReader,
{
    async fn get_chain_info(&self, _request: Request<()>) -> Result<Response<PbChainInfo>, Status> {
        let storage = self.storage.clone();

        let chain_info = tokio::task::spawn_blocking(move || {
            storage
                .read()
                .map_err(|e| Status::internal(e.to_string()))?
                .chain_state()
                .map_err(|e| Status::internal(e.to_string()))?
                .ok_or_else(|| Status::unavailable("storage is not initialized"))
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))??;

        let pb = PbChainInfo {
            height: chain_info.latest_block.0,
            latest_block_hash: chain_info.latest_block.1.to_string(),
            chain_work: chain_work_to_bytes(&chain_info.chain_work).to_vec(),
            last_update_time: chain_info.last_update_time,
            protocol_version: self.config.protocol_version as u32,
            chain_name: self.config.chain_name.clone(),
        };

        Ok(Response::new(pb))
    }

    type GetHeadersByHeightStream =
        Pin<Box<dyn Stream<Item = Result<PbBlockHeader, Status>> + Send + 'static>>;
    async fn get_headers_by_height(
        &self,
        request: Request<BlockHeightRange>,
    ) -> Result<Response<Self::GetHeadersByHeightStream>, Status> {
        let hashes = self.height_range_hashes(request.into_inner()).await?;
        let storage = self.storage.clone();

        let header_stream = stream::iter(hashes).then(move |hash| {
            let storage = storage.clone();

            async move {
                let block = Self::read_block(storage, hash).await?.ok_or_else(|| {
                    Status::internal(format!("block {hash} disappeared from storage"))
                })?;

                Ok(PbBlock::from(block).header.unwrap())
            }
        });

        Ok(Response::new(Box::pin(header_stream)))
    }

    type GetBlocksByHeightStream =
        Pin<Box<dyn Stream<Item = Result<PbBlock, Status>> + Send + 'static>>;
    async fn get_blocks_by_height(
        &self,
        request: Request<BlockHeightRange>,
    ) -> Result<Response<Self::GetBlocksByHeightStream>, Status> {
        let hashes = self.height_range_hashes(request.into_inner()).await?;
        let storage = self.storage.clone();

        let block_stream = stream::iter(hashes).then(move |hash| {
            let storage = storage.clone();

            async move {
                let block = Self::read_block(storage, hash).await?.ok_or_else(|| {
                    Status::internal(format!("block {hash} disappeared from storage"))
                })?;

                Ok(block.into())
            }
        });

        Ok(Response::new(Box::pin(block_stream)))
    }

    type GetBlocksByHashStream =
        Pin<Box<dyn Stream<Item = Result<PbBlock, Status>> + Send + 'static>>;

    async fn get_blocks_by_hash(
        &self,
        request: Request<BlockHashList>,
    ) -> Result<Response<Self::GetBlocksByHashStream>, Status> {
        let list = request.into_inner();
        let hashes = list.block_hashes;

        // Enforce a maximum
        let max = self.config.max_blocks_range_per_request;
        if hashes.len() > max {
            return Err(Status::invalid_argument("Too many block hashes requested"));
        }

        let storage = self.storage.clone();

        let block_stream = stream::iter(hashes).then(move |block_hash| {
            let storage = storage.clone();

            async move {
                let block_hash = BlockHash::from_hash_string(&block_hash)
                    .map_err(|e| Status::invalid_argument(format!("Invalid hash: {e:?}")))?;

                let block = Self::read_block(storage, block_hash)
                    .await?
                    .ok_or_else(|| Status::not_found("Block not found in storage"))?;

                Ok(block.into())
            }
        });

        // box and pin stream
        let pinned_stream = Box::pin(block_stream);

        Ok(Response::new(pinned_stream))
    }
}

impl<DB> StryiSyncService<DB>
where
    DB: StorageReader,
{
    async fn read_block(storage: DB, hash: BlockHash) -> Result<Option<Block>, Status> {
        tokio::task::spawn_blocking(move || {
            let view = storage
                .read()
                .map_err(|e| Status::internal(e.to_string()))?;

            view.get_block_by_hash(hash)
                .map_err(|e| Status::internal(e.to_string()))
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
    }

    async fn height_range_hashes(&self, range: BlockHeightRange) -> Result<Vec<BlockHash>, Status> {
        if range.start_height > range.end_height {
            return Err(Status::invalid_argument("start height exceeds end height"));
        }

        let max = self.config.max_blocks_range_per_request as u64;

        if max == 0 {
            return Err(Status::internal("maximum block range is zero"));
        }

        let end = range
            .end_height
            .min(range.start_height.saturating_add(max - 1));
        let storage = self.storage.clone();

        tokio::task::spawn_blocking(move || {
            let view = storage
                .read()
                .map_err(|e| Status::internal(e.to_string()))?;

            let mut hashes = Vec::new();

            for height in range.start_height..=end {
                let hash = view
                    .get_block_hash_by_height(height)
                    .map_err(|e| Status::internal(e.to_string()))?
                    .ok_or_else(|| {
                        Status::not_found(format!("Block at height {height} not found"))
                    })?;

                hashes.push(hash);
            }

            Ok(hashes)
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
    }
}

impl From<Block> for PbBlock {
    fn from(block: Block) -> PbBlock {
        let header = PbBlockHeader {
            version: block.header.version as u32,
            merkle_root_hash: block.header.merkle_root_hash.to_string(),
            previous_block_hash: block.header.previous_block_hash.to_string(),
            height: block.header.height,
            hash_threshold: threshold_to_bytes(&block.header.hash_threshold).to_vec(),
            timestamp: block.header.timestamp,
            nonce: block.header.nonce,
            genesis_data: postcard::to_stdvec(&block.header.genesis_state)
                .expect("I bet it won't ever happen 1"),
        };

        let body = SerializedBlockBody {
            serialized: postcard::to_stdvec(&block.data).expect("I bet it won't ever happen 2"),
        };

        PbBlock {
            header: Some(header),
            body: Some(body),
        }
    }
}

impl TryFrom<PbBlock> for Block {
    type Error = StryiCoreError;

    fn try_from(value: PbBlock) -> Result<Self, Self::Error> {
        // Try to extract the header from the PbBlock
        // Deserialize genesis_state
        let header: BlockHeader = {
            let grpc_header = value
                .header
                .ok_or(StryiCoreError::other("Got block with no header!"))?;

            // try to deserialize genesis_state.
            let grpc_genesis_state: Option<GenesisState> =
                postcard::from_bytes(grpc_header.genesis_data.as_slice())
                    .map_err(StryiCoreError::other)?;

            BlockHeader {
                version: u16::try_from(grpc_header.version).map_err(|_| {
                    StryiCoreError::other(format!(
                        "unsupported block version {} on the wire",
                        grpc_header.version
                    ))
                })?,
                merkle_root_hash: MerkleHash::from_hash_string(&grpc_header.merkle_root_hash)?,
                previous_block_hash: BlockHash::from_hash_string(&grpc_header.previous_block_hash)?,
                height: grpc_header.height,
                hash_threshold: threshold_from_bytes(&grpc_header.hash_threshold)
                    .map_err(StryiCoreError::other)?,
                timestamp: grpc_header.timestamp,
                nonce: grpc_header.nonce,
                genesis_state: grpc_genesis_state,
            }
        };

        // Decode the serialized block body
        let data = {
            let grpc_body = value
                .body
                .ok_or(StryiCoreError::other("Got block with no body!"))?;
            postcard::from_bytes(&grpc_body.serialized).map_err(StryiCoreError::other)?
        };

        // Construct the Block from header and body
        Ok(Block { header, data })
    }
}
