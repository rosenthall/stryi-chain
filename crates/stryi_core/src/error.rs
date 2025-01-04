use hex::FromHexError;
use thiserror::Error;


#[derive(Debug, Clone, Error)]
pub enum StryiCoreError {

    
    #[error("Unexpected prefix while trying decode hash. Actual : {actual:?}, expected : {expected:?}")]
    InvalidPrefix { expected: String, actual: String },

    #[error("Error while decoding hex value : {0:?}")]
    InvalidHex(#[from] FromHexError),

    #[error("Got unknown error : {msg:?}")]
    Other {
        msg : String
    },

    #[error("Unexpected buffer length trying decode hash. Actual : {actual:?}, expected : {expected:?}")]
    InvalidLength { expected: usize, actual: usize },
}