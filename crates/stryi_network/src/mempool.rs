use libp2p::request_response::cbor::Behaviour as RequestResponseBehaviour;
use libp2p::request_response::Message;
use serde::{Deserialize, Serialize};
use stryi_core::mempool::MemPoolSyncData;
use stryi_core::transactions::{Transaction, TransactionHash};

#[derive(Debug, Clone)]
pub struct MempoolProtocol;


#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum MempoolRequest {
    GetState,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum MempoolResponse {
    State(MemPoolSyncData),
}

/// Our mempool-related NetworkBehaviour relies on https://docs.rs/libp2p/latest/libp2p/request_response/cbor/type.Behaviour.html to perform serialization in binary format
pub type MempoolSyncBehaviour =  RequestResponseBehaviour<MempoolRequest, MempoolResponse>;

/// Definition of an inbound request or response for mempool
pub type MempoolMessage = Message<MempoolRequest, MempoolResponse>;