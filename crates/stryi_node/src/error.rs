use thiserror::Error;

#[derive(Error, Clone, Debug)]
pub enum StryiNodeError {
    #[error("Got unknown error : {msg:?}")]
    Other {
        msg : String
    },
}



impl StryiNodeError {
    /// Constructs simple `StryiNodeError::Other` instance with provided message
    pub fn other(msg: impl ToString) -> Self {
        StryiNodeError::Other {
            msg : msg.to_string()
        }
    }
}
