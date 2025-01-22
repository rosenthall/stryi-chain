use std::string::FromUtf8Error;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StryiStorageError {
    
    #[error("Cannot initialize storage because of incorrect path : {msg}")]
    IncorrectPath {msg : String},
    
    #[error("Fjall returned an error : {0}")]
    FjallError(#[from] fjall::Error),
    
    
    #[error("Bincode serialization error")]
    SerializationError(#[from] bincode::error::EncodeError),
    

    #[error("Bincode deserialization error")]
    DeserializationError(#[from] bincode::error::DecodeError),

    
    #[error("Error while trying construct typed hash object from string : {0}")]
    IncorrectHashValue(String),
    
    
    #[error("Error while converting bytes in string {0}")]
    FromUtf8Error(#[from] FromUtf8Error),
    
    #[error("Cannot find value with such key in data base: {0}")]
    NotFound(String),
}

