use thiserror::Error;

/// Node-level error type.
#[derive(Error, Debug)]
pub enum StryiNodeError {
    // key-handling layer
    #[error(transparent)]
    KeyEncode(#[from] stryi_network::SigningError),

    #[error(transparent)]
    KeyDecode(#[from] stryi_network::DecodingError),

    // I/O layer 
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Pkcs8(#[from] pkcs8::Error),

    #[error("Got http server error : {0}")]
    HttpServer(String),
    
    /// Single structured error for any chain-info mismatch during peer handshake.
    /// `field` is a stable key (e.g., "protocol_version", "chain_name", "tip_hash@same_height").
    /// Values are rendered as strings to avoid leaking types across layers.
    #[error("peer chain info mismatch: {field} (remote={remote}, local={local})")]
    PeerChainInfoMismatch {
        field:  &'static str,
        local:  String,
        remote: String,
    },


    // fallback / misc
    #[error("unexpected error: {0}")]
    Other(String),
}


impl StryiNodeError {
    #[inline]
    /// Constructs simple `StryiNodeError::Other` instance with provided message
    pub fn other(msg: impl ToString) -> Self {
        StryiNodeError::Other(msg.to_string())
    }    
    
    
    /// Constructs StryiNodeError::PeerChainInfoMismatch
    #[inline]
    pub fn chain_info_mismatch(
        field: &'static str,
        local: impl ToString,
        remote: impl ToString,
    ) -> Self {
        StryiNodeError::PeerChainInfoMismatch {
            field,
            local:  local.to_string(),
            remote: remote.to_string(),
        }
    }
}
