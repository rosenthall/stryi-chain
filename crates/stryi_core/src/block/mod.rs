mod block_hash;


pub use block_hash::{BlockHash};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Block {
    
    /// Private field representing nonce
    nonce : u32, // Bitcoin also uses 32 byte value as a nonce
    
    
    /// Unix timestamp value
    pub timestamp : u64,

    
    /// Number of bits for the mining complexity level
    pub bits : u8, 
    
    
    
    // TODO: Transaction, TransactionHash, UTXO system
}
