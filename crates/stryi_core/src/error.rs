use hex::FromHexError;
use thiserror::Error;
use crate::address::AccountAddress;
use crate::transactions::TransactionHash;

#[derive(Debug, Clone, Error, PartialEq)]
pub enum StryiCoreError {
    #[error("Unexpected prefix while trying decode hash. Actual : {actual:?}, expected : {expected:?}")]
    InvalidPrefix { expected: String, actual: String },

    #[error("Error while decoding hex value : {0:?}")]
    InvalidHex(#[from] FromHexError),

    #[error("Unexpected buffer length trying decode hash. Actual : {actual:?}, expected : {expected:?}")]
    InvalidLength { expected: usize, actual: usize },


    #[error("Invalid transaction-level signature")]
    TxInvalidSignature,

    #[error("Missing UTXO for txid={txid}, vout={vout}")]
    TxMissingUtxo {
        txid: TransactionHash,
        vout: u32,
    },

    #[error("UTXO owned by {expected:?}, not {actual:?}")]
    TxWrongOwner {
        expected: AccountAddress,
        actual: AccountAddress,
    },

    #[error("Insufficient input value: sum(inputs)={input_sum}, sum(outputs)={output_sum}")]
    TxInsufficientInputValue {
        input_sum: u64,
        output_sum: u64,
    },
    
    
    #[error("Invalid signature : {msg}")]
    InvalidSignature { 
        msg : String
    },
    
    #[error("Block validation failed: {details}")]
    ConsensusValidationFailed {
        details: String,
    },

    #[error("Failed to apply valid block to current state: {details}")]
    ConsensusBlockApplyingFailed {
        details: String,
    },


    #[error("Difficulty adjustment failed: {details}")]
    ConsensusDifficultyAdjustmentFailed {
        details: String,
    },
    
    #[error("Chain selection failed: {details}")]
    ConsensusChainSelectionFailed {
        details: String,
    },

    #[error("Invalid coinbase reward amount: maximal expected was: {max_expected}, got: {actual}")]
    ConsensusInvalidCoinbaseAmount {
        max_expected : u64,
        actual : u64
    },

     #[error("Invalid difficulty value : {details}")]
    InvalidDifficultyValue {
        details: String
    },

    
    #[error("Error while processing transactions' dependency tree : {msg}")]
    TransactionDependencyError { msg: String },

    
    #[error("Detected transaction tries to perform double spend : {txid}:{vout}")]
    TxDoubleSpend { txid: TransactionHash, vout: u32 },

    #[error("Got unknown error : {msg:?}")]
    Other {
        msg : String
    },

}

impl StryiCoreError {
    
    /// Constructs simple `StryiCoreError::Other` instance with provided message
    pub fn other(msg: impl ToString) -> Self {
        StryiCoreError::Other {
            msg : msg.to_string()
        } 
    }
}
