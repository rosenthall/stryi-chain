use std::pin::Pin;
use tonic::{Request, Response, Status};
use tonic::codegen::tokio_stream::Stream;
use crate::grpc_sync::{
    
    // Some renaming
    Block as PbBlock,
    BlockHeader as PbBlockHeader,
    ChainInfo as PbChainInfo, 
    
    MempoolRequest, BlockHeightRange,  BlockHashList, SerializedTransaction
};


/// Implementation of grpc sync protocol, see protos/sync.proto
pub struct StryiSyncService {
    /// Basic configuration fields, like version of the protocol or the name of chain
    config: StryiSyncServiceConfig
}


pub struct StryiSyncServiceConfig {

    /// Name of this exact chain and network, e.g `testnet`, `stryichain`, whatever
    chain_name: String,

    /// Numerical value that represents version of sync protocol
    protocol_version : usize,

    /// Value to avoid asking for entire chain quickly.
    max_blocks_range_per_request : usize,
}


#[tonic::async_trait] 
impl crate::grpc_sync::blockchain_sync_server::BlockchainSync for StryiSyncService {
    async fn get_chain_info(&self, request: Request<()>) -> Result<Response<PbChainInfo>, Status> {
        todo!()
    }

    type GetHeadersByHeightStream = Pin<Box<dyn Stream<Item = Result<PbBlockHeader, Status>> + Send + 'static>>;
    async fn get_headers_by_height(&self, request: Request<BlockHeightRange>) -> Result<Response<Self::GetHeadersByHeightStream>, Status> {
        todo!()
    }

    type GetBlocksByHeightStream = Pin<Box<dyn Stream<Item = Result<PbBlock, Status>> + Send + 'static>>;
    async fn get_blocks_by_height(&self, request: Request<BlockHeightRange>) -> Result<Response<Self::GetBlocksByHeightStream>, Status> {
        todo!()
    }

    type GetBlocksByHashStream = Pin<Box<dyn Stream<Item = Result<PbBlock, Status>> + Send + 'static>>;
    async fn get_blocks_by_hash(&self, request: Request<BlockHashList>) -> Result<Response<Self::GetBlocksByHashStream>, Status> {
        todo!()
    }

 
    type GetMempoolTransactionsStream = Pin<Box<dyn Stream<Item = Result<SerializedTransaction, Status>> + Send + 'static>>;
    async fn get_mempool_transactions(&self, request: Request<MempoolRequest>) -> Result<Response<Self::GetMempoolTransactionsStream>, Status> {
        todo!()
    }
}
