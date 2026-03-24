use crate::StryiEvent;
use libp2p::request_response::Event as ReqRespEvent;
use libp2p::request_response::cbor::Behaviour as RequestResponseBehaviour;
use serde::{Deserialize, Serialize};
use stryi_core::mempool::MemPoolSyncData;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum MempoolRequest {
    GetState,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum MempoolResponse {
    State(MemPoolSyncData),
}

// CBOR request-response behavior used for mempool sync
pub type MempoolSyncBehaviour = RequestResponseBehaviour<MempoolRequest, MempoolResponse>;

/// Definition of an inbound request or response for mempool
pub type MempoolEvent = ReqRespEvent<MempoolRequest, MempoolResponse>;

impl From<MempoolEvent> for StryiEvent {
    fn from(e: MempoolEvent) -> Self {
        StryiEvent::Mempool(e)
    }
}
