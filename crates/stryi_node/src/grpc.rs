use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use futures_util::stream;
use tokio::sync::RwLock;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};
use tonic::codegen::tokio_stream::Stream;
use tower::{Layer, Service};
use stryi_core::block::{Block, BlockHash};
use stryi_core::storage::{BlockStorage, UtxoStorage};
use stryi_storage::StryiStorage;
use http::{Request as HttpRequest, Response as HttpResponse, StatusCode};
use tonic::body::Body;
use crate::grpc_services::{
    // Some aliases to avoid overlapping with similar structs from stryi_core 
    Block as PbBlock,
    BlockHeader as PbBlockHeader,
    ChainInfo as PbChainInfo,
    BlockHeightRange, BlockHashList, SerializedBlockBody
};


/// Implementation of grpc sync protocol, see protos/sync.proto
#[derive(Clone)]
pub struct StryiSyncService<DB>
where DB:
    BlockStorage + UtxoStorage
{
    /// Basic configuration fields, like version of the protocol or the name of chain
    pub(crate) config: StryiSyncServiceConfig,

    /// Arc'd storage reference
    pub(crate) storage : Arc<RwLock<DB>>,
}

#[derive(Clone, Debug)]
pub struct StryiSyncServiceConfig {

    pub(crate) address : SocketAddr,
    
    /// Name of this exact chain and network, e.g `testnet`, `stryichain`, whatever
    pub(crate) chain_name: String,

    /// Numerical value that represents version of sync protocol
    pub(crate) protocol_version : usize,

    /// Value to avoid asking for entire chain quickly.
    pub(crate) max_blocks_range_per_request : usize,
}


/// A layer that adds "readiness" checking to any service.
/// `is_ready`=false means the service will return a UNAVAILABLE response.
/// You can later set `is_ready` to `true` to let requests pass through.
#[derive(Debug, Clone, Default)]
pub struct ReadinessMiddlewareLayer {
    is_ready : Arc<RwLock<bool>>,
}

impl ReadinessMiddlewareLayer {
    pub fn new(shared_flag: Arc<RwLock<bool>>) -> Self {
        ReadinessMiddlewareLayer {
            is_ready: shared_flag,
        }
    }
}


impl<S> Layer<S> for ReadinessMiddlewareLayer {
    type Service = ReadinessMiddleware<S>;


    /// Wraps the given service `S` in a `ReadinessMiddleware`, injecting
    /// an Arc<RwLock<bool>> to track whether the node is "ready."
    fn layer(&self, service: S) -> Self::Service {
        ReadinessMiddleware {
            inner: service,
            is_ready: self.is_ready.clone(), 
        }
    }
}

/// A pinned, boxed future type alias.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The middleware struct itself, holding the inner service and
/// a shared readiness state.
#[derive(Debug, Clone)]
pub struct ReadinessMiddleware<S> {
    /// The underlying service we’re wrapping.
    pub inner: S,
    /// A shared boolean indicating whether this node is ready to serve requests.
    pub is_ready: Arc<RwLock<bool>>,
}

impl<S, ReqBody> Service<HttpRequest<ReqBody>> for ReadinessMiddleware<S>
where

    // The inner service must produce `HttpResponse<Body>` to match our short-circuit response.
    S: Service<HttpRequest<ReqBody>, Response = HttpResponse<Body>> + Clone + Send + 'static,
    // The future from the inner service must be `Send` + 'static.
    S::Future: Send + 'static,
    // The request body must be `Send` + 'static.
    ReqBody: Send + 'static,
{
    // just use the same response and error types
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;


    /// Forwards readiness checks to the inner service's readiness.
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    /// The core call method: if `is_ready` is false, return an UNAVAILABLE. Otherwise, call `inner`
    fn call(&mut self, req: HttpRequest<ReqBody>) -> Self::Future {
        let clone_inner = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone_inner);
        let readiness_flag = self.is_ready.clone();

        Box::pin(async move {
            
            // check the flag
            let ready = {
                let guard = readiness_flag.read().await;
                *guard
            };

            // if not ready - return UNAVAILABLE
            if !ready {
                
                // Construct a gRPC "UNAVAILABLE" error response
                let grpc_error = tonic::codegen::http::Response::builder()
                    .status(StatusCode::OK) // gRPC specs typically use 200 OK here, and rely on the grpc-status header
                    .header("content-type", "application/grpc")
                    .header("grpc-status", "14") // 14 is "UNAVAILABLE" per https://github.com/grpc/grpc/blob/master/doc/statuscodes.md
                    .header("grpc-message", "Node is not synchronized yet. Try again later")
                    .body(Body::empty())
                    .unwrap();

                return Ok(grpc_error);

            }

            // If ready, delegate the request to the underlying service.
            let response = inner.call(req).await?;
            Ok(response)
        })
    }
}


#[tonic::async_trait]
impl crate::grpc_services::blockchain_sync_server::BlockchainSync for StryiSyncService<StryiStorage> {
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
            protocol_version: self.config.protocol_version.clone() as u32,
            chain_name: self.config.chain_name.clone(),
        };

        Ok(Response::new(pb))

    }

    type GetHeadersByHeightStream = Pin<Box<dyn Stream<Item = Result<PbBlockHeader, Status>> + Send + 'static>>;
    async fn get_headers_by_height(
        &self,
        request: Request<BlockHeightRange>,
    ) -> Result<Response<Self::GetHeadersByHeightStream>, Status> {

        let params = request.into_inner();
        let start_height = params.start_height;
        let end_height   = params.end_height;

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
                let block_result = db.get_block_by_height(current_height as usize).await;

                match block_result {
                    Ok(block) => {
                        // Convert the header to PbBlockHeader
                        let pb_header: PbBlockHeader = PbBlock::from(block).header.unwrap(); // safe unwrap
                        let next_state = current_height + 1;
                        Some((Ok(pb_header), next_state))
                    }
                    Err(e) => {
                        // Return a single error item and end the stream
                        let status = if matches!(e, stryi_storage::StryiStorageError::NotFound(_)) {
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

    type GetBlocksByHeightStream = Pin<Box<dyn Stream<Item = Result<PbBlock, Status>> + Send + 'static>>;
    async fn get_blocks_by_height(
        &self,
        request: Request<BlockHeightRange>,
    ) -> Result<Response<Self::GetBlocksByHeightStream>, Status> {
        // 1) Parse request
        let params = request.into_inner();
        let start_height = params.start_height;
        let end_height   = params.end_height;

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
                let block_res = db.get_block_by_height(current_height as usize).await;

                match block_res {
                    Ok(block) => {
                        // Convert to protobuf type
                        let pb_block: PbBlock = block.into();
                        
                        // update acc
                        let next_state = current_height + 1;
                        
                        // (Item, NextState)
                        Some((Ok(pb_block), next_state))
                    }
                    Err(e) => {
                        // If a block is missing or any error occurred, 
                        // produce an error item. Once Tonic sees an error,
                        // the stream ends and the client receives that error.
                        let status = if matches!(e, stryi_storage::StryiStorageError::NotFound(_)) {
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


    type GetBlocksByHashStream = Pin<Box<dyn Stream<Item = Result<PbBlock, Status>> + Send + 'static>>;

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
        let block_stream = stream::iter(hashes)
            .then(move |block_hash| {
                let storage = storage.clone();
                async move {


                    // Convert string hash to BlockHash
                    let block_hash = BlockHash::from_hash_string(&block_hash)
                        .map_err(|e| Status::invalid_argument(format!("Invalid hash: {e:?}")))?;

                    // 2) Read from storage
                    let store = storage.read().await;
                    let block = match store.get_block_by_hash(block_hash).await {
                        Ok(b) => b,
                        Err(e) => {
                            if matches!(e, stryi_storage::StryiStorageError::NotFound(_)) {
                                Err(Status::not_found("Block not found in storage"))?
                            } else {
                                Err(Status::internal(e.to_string()))?
                            }
                        }
                    };

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
            merkle_root_hash: block.header.merkle_root_hash.to_vec(),
            previous_block_hash: block.header.previous_block_hash.to_string(),
            height: block.header.height,
            difficulty_bits: block.header.difficulty_bits as u32,
            timestamp: block.header.timestamp,
            nonce: block.header.nonce,
            is_genesis: block.header.is_genesis,
        };

        let body = SerializedBlockBody {
            serialized: bincode::serde::encode_to_vec(&block.data, bincode::config::standard()).expect("I bet it won't ever happen")
        };

        PbBlock {
            header : Some(header),
            body:  Some(body)
        }
    }
}