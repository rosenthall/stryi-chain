use serde::{Deserialize, Serialize};
use thiserror::Error;
use std::fmt;
use stryi_core::address::AccountAddress;
use stryi_core::transactions::{OutPoint, TransactionHash};

/// Error type for StryiNode HTTP API
#[derive(Debug, Serialize, Deserialize, Eq, PartialEq, Clone, Error)]
#[serde(rename_all = "snake_case")]
pub enum StryiNodeHttpApiErrorKind {
    #[error("The transaction is invalid or malformed: {0}")]
    BadTransaction(String),

    #[error("The requested {0} was not found: {1}")]
    ResourceNotFound(ResourceKind, String),
}

/// Represents a kind of resource that can be requested from the StryiNode HTTP API.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ResourceKind {
    Block(u64),
    Transaction(TransactionHash),
    Account(AccountAddress),
    Utxo(OutPoint),
}


impl ResourceKind {
    pub fn as_str(&self) -> String {
        match self {
            // 'Block {height}' for blocks
            ResourceKind::Block(height) => format!("Block {}", height),
            // 'Account {partial_hash}' for accounts
            // where partial_hash is the first 7 characters and last 6 characters of the account
            // hash, e.g. 'Account @123456...789abc'
            ResourceKind::Account(hash) => {
                let hash_str = hash.to_string();
                let mut chars = hash_str.chars();
                let prefix: String = chars.by_ref().take(7).collect();
                let suffix: String = hash_str.chars().rev().take(6).collect::<String>().chars().rev().collect();
                format!("Account {}...{}", prefix, suffix)
            }
            // 'Transaction {hash}' for transactions
            ResourceKind::Transaction(hash) => format!("Transaction {}", hash),
            // 'Utxo {tx_hash}:{vout}' for UTXOs
            ResourceKind::Utxo(outpoint) => format!("Utxo {}:{}", outpoint.txid, outpoint.vout),
        }
    }
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}


#[cfg(test)]
mod tests {
    use stryi_core::address::AccountAddress;

    #[test]
    fn display_resource_kind_account() {
        use super::ResourceKind;
        let address = AccountAddress::from_hash_string("@dd0ca8155d946853106a5a5fb126ce17fdd9136c")
            .expect("This should be a valid account address");
        let account_resource = ResourceKind::Account(address);
        assert_eq!(account_resource.to_string(), "Account @dd0ca8...d9136c");
    }
}