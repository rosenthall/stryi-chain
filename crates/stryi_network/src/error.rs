use bincode::error::DecodeError;
use libp2p::request_response::InboundRequestId;
use thiserror::Error;
use tokio::sync::broadcast::error::SendError;
use stryi_core::mempool::MemPoolError;
use crate::{NetworkCommand, NetworkEvent};

/// Our custom error type for networking logic.
#[derive(Debug, Error)]
pub enum StryiNetworkError {
    /// Some transport-related or I/O error.
    #[error("Transport error: {0}")]
    Transport(String),

    /// We failed to parse a multiaddr or something similar.
    #[error("Address parse error: {0}")]
    ParseAddr(String),

    /// Rendezvous usage error, e.g. registration or discover issue.
    #[error("Rendezvous error: {0}")]
    Rendezvous(String),
    
    /// Noise configuration construction error
    #[error("Cannot construct noise configuration instance : {0}")]
    NoiseConfigError(libp2p::noise::Error),
    
    #[error("Cannot construct gossipsub config, error: {0}")]
    GossipsubConfigError(libp2p::gossipsub::ConfigBuilderError),

    /// Wrapper around tokio::sync::mpsc::error for our NetworkManager
    #[error("Cannot send NetworkCommand to NetworkManager: {0}")]
    ChannelError(SendError<NetworkCommand>),
    
    /// Mempool-related errors
    #[error("Got mempool error : {0}")]
    MempoolError(MemPoolError),
    
    #[error("Cannot respond on request with id: {0}")]
    CannotRespond(InboundRequestId),
    
    #[error("Cannot decode message from gossipsub : {0:?}")]
    DecodeGossipsubMessageError(DecodeError),
    
    #[error("Stryi-NetworkManager cannot send NetworkEvent, error: {0}")]
    CannotSendEvent(SendError<NetworkEvent>),
    
    /// Catch-all fallback.
    #[error("Other networking error: {0}")]
    Other(String),
}

impl StryiNetworkError {
    pub fn other(msg: impl ToString) -> Self {
        StryiNetworkError::Other(msg.to_string())
    }
}
