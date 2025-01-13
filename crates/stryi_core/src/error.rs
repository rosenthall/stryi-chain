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
}