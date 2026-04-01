use crate::address::AccountAddress;
use crate::transactions::hash::TransactionHash;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

/// OutPoint identifies which UTXO is being referenced.
/// Basically it's a handle for the UTXOs
#[derive(Debug, Serialize, Deserialize, Clone, Copy, Eq, Hash, PartialEq)]
pub struct OutPoint {
    /// Transaction hash that created the output.
    pub txid: TransactionHash,
    /// Output index within that transaction.
    pub vout: u32,
}

impl Display for OutPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.txid, self.vout)
    }
}

/// TransactionIn represents an input of the transaction,
/// referencing an existing UTXO to be spent.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct TransactionIn {
    /// Which UTXO is being spent
    pub previous_output: OutPoint,

    /// Sequence field (similar to Bitcoin).
    /// It's optional, only for advanced use (locktime, etc.)
    pub sequence: u32,
}

impl Display for TransactionIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.previous_output)
    }
}

/// TransactionOut represents an output of the transaction.
/// It includes an amount (value) and a recipient address.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct TransactionOut {
    /// Amount assigned to this output.
    pub value: u64,
    /// Address allowed to spend this output.
    pub recipient: AccountAddress,
}

/// UTXO (Unspent Transaction Output) is a spendable output.
#[derive(Debug, Serialize, Eq, PartialEq, Hash, Deserialize, Clone, Copy)]
pub struct UTXO {
    /// Transaction hash that created the output.
    pub txid: TransactionHash,

    /// Output index within that transaction.
    pub vout: u32,

    /// Amount carried by this UTXO.
    pub value: u64,

    /// Address currently owning the UTXO.
    pub owner: AccountAddress,
}
