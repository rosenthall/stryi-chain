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

    // fallback / misc
    #[error("unexpected error: {0}")]
    Other(String),
}



impl StryiNodeError {
    /// Constructs simple `StryiNodeError::Other` instance with provided message
    pub fn other(msg: impl ToString) -> Self {
        StryiNodeError::Other(msg.to_string())
    }
}
