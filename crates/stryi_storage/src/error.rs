use std::string::FromUtf8Error;
use thiserror::Error;
use stryi_core::storage::RangeError;

#[derive(Debug, Error)]
pub enum StryiStorageError {
    
    #[error("Cannot initialize storage because of incorrect path : {msg}")]
    IncorrectPath {msg : String},
    
    #[error("Fjall returned an error : {0}")]
    FjallError(#[from] fjall::Error),
    
    #[error("Database is not initialized and no configuration for setting up provided")]
    NoInitializationConfigProvided,
    
    #[error("Bincode serialization error")]
    SerializationError(#[from] bincode::error::EncodeError),
    
    #[error("Bincode deserialization error")]
    DeserializationError(#[from] bincode::error::DecodeError),
    
    #[error("Error while trying construct typed hash object from bytes, message : {0}")]
    IncorrectHashValue(String),
    
    #[error("Nonexistent height value was provided : {0}")]
    InvalidHeight(usize),
    
    #[error("Error while converting bytes in string {0}")]
    FromUtf8Error(#[from] FromUtf8Error),
    
    #[error("Cannot get storage stats from `stats_partition` : {0}")]
    NoStorageStatsFound(String),

    #[error("Incorrect blocks range provided: start={0}, end={1}")]
    IncorrectBlocksRange(i32, i32),

    #[error("Cannot find value with such key in data base: {0}")]
    NotFound(String),
    
    #[error("Cannot construct undo object for the block: {msg}")]
    UndoCreationError { msg: String },
}




impl From<RangeError> for StryiStorageError {
    fn from(e: RangeError) -> Self {
        match e {   
            RangeError::InvalidRange { start, end } => 
                StryiStorageError::IncorrectBlocksRange(start, end),
        }
    }
}
