use thiserror::Error;

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

    /// Catch-all fallback.
    #[error("Other networking error: {0}")]
    Other(String),
}

impl StryiNetworkError {
    pub fn other(msg: impl ToString) -> Self {
        StryiNetworkError::Other(msg.to_string())
    }
}
