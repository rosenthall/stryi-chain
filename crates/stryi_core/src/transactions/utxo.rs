use serde::{Deserialize, Serialize};
use crate::address::AccountAddress;
use crate::transactions::hash::TransactionHash;
use crate::transactions::signature::StryiSignature;

/// OutPoint identifies which UTXO is being referenced:
/// - `txid`: the transaction hash (32-byte typed hash)
/// - `vout`: the index of the output within that transaction
#[derive(Debug, Serialize, Deserialize, Clone, Eq, Hash, PartialEq)]
pub struct OutPoint {
    /// The transaction identifier as a typed hash
    pub txid: TransactionHash,
    /// Index of the output within the transaction
    pub vout: u32,
}

/// TransactionIn represents an input of the transaction.
/// We don't use any scripting system, so a simple `signature` vector is enough
/// to prove ownership if needed at the input level. This can be expanded for
/// multi-input owners, etc.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TransactionIn {
    /// Which UTXO is being spent
    pub previous_output: OutPoint,
    
    /// Sender's signature to authorize spending this particular UTXO
    pub signature: StryiSignature,
    
    /// Sequence field (similar to Bitcoin, can be used for additional logic)
    pub sequence: u32,
}

/// TransactionOut represents an output of the transaction.
/// It simply includes an amount (value) and a recipient identifier (address).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TransactionOut {
    /// Amount of "coins" to send
    pub value: u64,
    /// Recipient address
    pub recipient: AccountAddress,
}

/// UTXO (Unspent Transaction Output) is a spendable output.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UTXO {
    /// The transaction hash that created this output
    pub txid: TransactionHash,
    /// The index of this output within that transaction
    pub vout: u32,
    /// Amount of coins associated with this UTXO
    pub value: u64,
    /// Owner of the UTXO, e.g. an address or public key
    pub owner: AccountAddress,
}
