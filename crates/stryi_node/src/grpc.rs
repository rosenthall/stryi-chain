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
use bincode::config::standard;
use futures_util::stream;
use http::{Response as HttpResponse, StatusCode};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use stryi_core::StryiCoreError;
use stryi_core::block::{Block, BlockHash, BlockHeader, GenesisState};
use stryi_core::merkletree::MerkleHash;
use stryi_core::storage::{BlockStorage, UtxoStorage};
use stryi_storage::{StryiStorage, StryiStorageError};
use tokio::sync::RwLock;
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
    DB: BlockStorage + UtxoStorage,
{
    /// Basic configuration fields, like version of the protocol or the name of chain
    pub(crate) config: StryiSyncServiceConfig,

    /// Arc'd storage reference
    pub(crate) storage: Arc<RwLock<DB>>,
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
impl crate::grpc_services::blockchain_sync_server::BlockchainSync
    for StryiSyncService<StryiStorage>
{
    async fn get_chain_info(&self, _request: Request<()>) -> Result<Response<PbChainInfo>, Status> {
        // read storage stats entry in db
        let chain_info = self
            .storage
            .read()
            .await
            .get_current_storage_state()
            .map_err(|e| Status::internal(e.to_string()))?;

        // Construct PbChainInfo from StorageStateInformation we got and some values from self.config
        let pb = PbChainInfo {
            height: chain_info.latest_block.0 as u64,
            latest_block_hash: chain_info.latest_block.1.to_string(),
            total_difficulty: chain_info.chain_difficulty as u64,
            last_update_time: chain_info.last_update_time as u64,
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
        let params = request.into_inner();
        let start_height = params.start_height;
        let end_height = params.end_height;

        // Enforce max range
        let max = self.config.max_blocks_range_per_request as u64;
        let actual_end = std::cmp::min(end_height, start_height + max);

        let storage = self.storage.clone();

        // Build a stream using `futures_util::stream::unfold(...)`
        let header_stream = stream::unfold(start_height, move |current_height| {
            let storage = storage.clone();

            async move {
                // End the stream if current_height > actual_end
                if current_height > actual_end {
                    return None;
                }

                // Attempt to fetch the block
                let db = storage.read().await;
                let block_result = db.get_block_by_height(current_height).await;

                match block_result {
                    Ok(Some(block)) => {
                        let pb_header: PbBlockHeader = PbBlock::from(block).header.unwrap();
                        let next_state = current_height + 1;
                        Some((Ok(pb_header), next_state))
                    }

                    // Both “failed to load” cases in one arm
                    other => {
                        // Turn the match‐arm payload into a concrete error
                        let e: StryiStorageError = match other {
                            Err(e) => e,
                            Ok(None) => StryiStorageError::NotFound(format!(
                                "Block at height {} missing",
                                current_height
                            )),

                            // We’ve covered Ok(Some) above, so it should be impossible
                            _ => unreachable!(),
                        };

                        let status = if matches!(e, StryiStorageError::NotFound(_)) {
                            Status::not_found(e.to_string())
                        } else {
                            Status::internal(e.to_string())
                        };
                        Some((Err(status), actual_end + 1))
                    }
                }
            }
        });

        let pinned_stream = Box::pin(header_stream);
        Ok(Response::new(pinned_stream))
    }

    type GetBlocksByHeightStream =
        Pin<Box<dyn Stream<Item = Result<PbBlock, Status>> + Send + 'static>>;
    async fn get_blocks_by_height(
        &self,
        request: Request<BlockHeightRange>,
    ) -> Result<Response<Self::GetBlocksByHeightStream>, Status> {
        // 1) Parse request
        let params = request.into_inner();
        let start_height = params.start_height;
        let end_height = params.end_height;

        // Enforce maximum range
        let max = self.config.max_blocks_range_per_request as u64;
        let actual_end = std::cmp::min(end_height, start_height + max);

        // We'll produce blocks for heights in [start_height..=actual_end].
        // If any block is missing, we return an error in the stream
        // and effectively terminate that stream.

        let storage = self.storage.clone();

        // 2) Build a stream using `futures_util::stream::unfold(...)`
        let block_stream = stream::unfold(start_height, move |current_height| {
            let storage = storage.clone(); // cloning arc
            async move {
                if current_height > actual_end {
                    // Reached the end, so no more items
                    return None;
                }

                // Lock DB and fetch the block
                let db = storage.read().await;
                let block_res = db.get_block_by_height(current_height).await;

                match block_res {
                    // Only match when we actually got a `Block`
                    Ok(Some(block)) => {
                        // Now `block: Block` matches your `From<Block>` impl
                        let pb_block: PbBlock = block.into();
                        let next_state = current_height + 1;
                        Some((Ok(pb_block), next_state))
                    }

                    // Block was not found in storage → translate to a gRPC NotFound error
                    Ok(None) => {
                        let status = Status::not_found(format!(
                            "Block at height {} not found",
                            current_height
                        ));
                        Some((Err(status), actual_end + 1))
                    }

                    // Storage API returned some other error
                    Err(e) => {
                        // If a block is missing or any error occurred,
                        // produce an error item. Once Tonic sees an error,
                        // the stream ends and the client receives that error.

                        let status = if matches!(e, StryiStorageError::NotFound(_)) {
                            Status::not_found(e.to_string())
                        } else {
                            Status::internal(e.to_string())
                        };

                        // We yield Some((Err(status), <dummy next state>))
                        // to produce exactly one error item, then effectively end
                        // by jumping past 'actual_end'
                        Some((Err(status), actual_end + 1))
                    }
                }
            }
        });

        // 3) Box the stream into Pin<Box<...>> so Tonic can return it
        let pinned_stream = Box::pin(block_stream);
        Ok(Response::new(pinned_stream))
    }

    type GetBlocksByHashStream =
        Pin<Box<dyn Stream<Item = Result<PbBlock, Status>> + Send + 'static>>;

    async fn get_blocks_by_hash(
        &self,
        request: Request<BlockHashList>,
    ) -> Result<Response<Self::GetBlocksByHashStream>, Status> {
        let list = request.into_inner();
        let hashes = list.block_hashes; // Vector of raw bytes for each hash

        // Enforce a maximum
        let max = self.config.max_blocks_range_per_request;
        if hashes.len() > max {
            return Err(Status::invalid_argument("Too many block hashes requested"));
        }

        // We will build a stream of results by iterating over `hashes`.
        // Clone the Arc so we can move it into the closure.
        let storage = self.storage.clone();

        // Produce a stream by iterating over each raw hash. For each hash, we do an async fetch
        // to get the block from storage, then return Ok(pb_block) or an Err(Status).
        let block_stream = stream::iter(hashes).then(move |block_hash| {
            let storage = storage.clone();
            async move {
                // Convert string hash to BlockHash
                let block_hash = BlockHash::from_hash_string(&block_hash)
                    .map_err(|e| Status::invalid_argument(format!("Invalid hash: {e:?}")))?;

                // 2) Read from storage
                let store = storage.read().await;

                // 2) fetch Option<Block> from storage
                let opt_block = store.get_block_by_hash(block_hash).await.map_err(|e| {
                    if matches!(e, StryiStorageError::NotFound(_)) {
                        Status::not_found("Block not found in storage")
                    } else {
                        Status::internal(e.to_string())
                    }
                })?;

                let block =
                    opt_block.ok_or_else(|| Status::not_found("Block not found in storage"))?;

                // 3) Convert to PbBlock
                let pb_block: PbBlock = block.into();

                // 4) Return it
                Ok(pb_block)
            }
        });

        // box and pin stream
        let pinned_stream = Box::pin(block_stream);

        Ok(Response::new(pinned_stream))
    }
}

impl From<Block> for PbBlock {
    fn from(block: Block) -> PbBlock {
        let header = PbBlockHeader {
            version: block.header.version as u32,
            merkle_root_hash: block.header.merkle_root_hash.to_string(),
            previous_block_hash: block.header.previous_block_hash.to_string(),
            height: block.header.height,
            difficulty_bits: block.header.difficulty_bits as u32,
            timestamp: block.header.timestamp,
            nonce: block.header.nonce,
            genesis_data: bincode::serde::encode_to_vec(block.header.genesis_state, standard())
                .expect("I bet it won't ever happen 1"),
        };

        let body = SerializedBlockBody {
            serialized: bincode::serde::encode_to_vec(&block.data, standard())
                .expect("I bet it won't ever happen 2"),
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
                bincode::serde::decode_from_slice(grpc_header.genesis_data.as_slice(), standard())
                    .map_err(StryiCoreError::other)?
                    .0;

            BlockHeader {
                version: grpc_header.version as u16,
                merkle_root_hash: MerkleHash::from_hash_string(&grpc_header.merkle_root_hash)?,
                previous_block_hash: BlockHash::from_hash_string(&grpc_header.previous_block_hash)?,
                height: grpc_header.height,
                difficulty_bits: grpc_header.difficulty_bits as u8,
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
            bincode::serde::decode_from_slice(&grpc_body.serialized, standard())
                .map_err(StryiCoreError::other)?
                .0
        };

        // Construct the Block from header and body
        Ok(Block { header, data })
    }
}
