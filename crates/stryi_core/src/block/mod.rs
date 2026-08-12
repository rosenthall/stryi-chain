mod block_hash;
pub(crate) mod mining;

pub use block_hash::BlockHash;
pub use mining::meets_difficulty;
use std::collections::HashMap;

use crate::address::AccountAddress;
use crate::consensus::ConsensusConsts;
use crate::merkletree::{MerkleHash, MerkleTree};
use crate::transactions::{
    StryiSignature, Transaction, TransactionData, TransactionKind, TransactionOut,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockData {
    /// List of transactions included in this block
    pub transactions: Vec<Transaction>,
}

/// Block header contains essential metadata for a blockchain block.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockHeader {
    /// Protocol/format version
    pub version: u16,

    /// Root of the Merkle tree of the block's transactions
    pub merkle_root_hash: MerkleHash,

    /// Hash of the previous block in the chain
    pub previous_block_hash: BlockHash,

    /// Block height (index in the chain)
    pub height: u64,

    /// Difficulty parameter in bits
    /// In genesis always must be 0.
    pub difficulty_bits: u8,

    /// Unix timestamp
    pub timestamp: u64,

    /// Nonce (in Bitcoin it's 32 bits so it's also enough for StryiChain)
    pub nonce: u32,

    /// Optional additional data for genesis blocks
    /// **If it is Some(_) - the block is considered genesis.**
    pub genesis_state: Option<GenesisState>,
}

impl BlockHeader {
    /// returns true if self.genesis_state is Some.
    pub const fn is_genesis(&self) -> bool {
        self.genesis_state.is_some()
    }
}

/// Configuration, the entire later chain relies on.
/// Stored in genesis, never changes.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenesisState {
    /// Consensus parameters.
    #[serde(rename = "consts")]
    pub consensus_consts: ConsensusConsts,
    // TODO: Some Additional fields in genesis_state? We may add any configurations/kill switches in here. Or maybe just string with info about the block?
}

/// Block ties together BlockHeader and BlockData.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Block {
    /// The block header
    pub header: BlockHeader,

    /// The block data (transactions)
    pub data: BlockData,
}

impl Block {
    /// Creates a new block with a given list of transactions, previous block hash, height, etc.
    /// Builds Merkle root from the provided transactions.
    ///
    /// Note: nonce is set to 0 by default, can be changed in a mining process.
    /// Note: this method is for non-genesis blocks. For those you have to use [`Self::new_genesis`] method
    pub fn new(
        transactions: Vec<Transaction>,
        previous_block_hash: BlockHash,
        height: u64,
        bits: u8,
        timestamp: u64,
        version: u16,
    ) -> Self {
        // Compute the Merkle root from the transactions
        let merkle_hash = Self::compute_merkle_root(&transactions);

        let header = BlockHeader {
            version,
            merkle_root_hash: merkle_hash,
            previous_block_hash,
            height,
            difficulty_bits: bits,
            timestamp,
            nonce: 0,
            genesis_state: None,
        };

        let data = BlockData { transactions };

        Self { header, data }
    }

    /// Creates a genesis block from the provided initial balances.
    /// `wanted_balances` maps each `AccountAddress` to its starting balance.
    /// The balances are converted into transaction outputs and sorted in descending order.
    /// The Merkle root is computed from the resulting genesis transaction.
    pub fn new_genesis(
        version: u16,
        wanted_balances: HashMap<AccountAddress, u64>,
        genesis_state: GenesisState,
    ) -> Self {
        // convert balances to TxOuts
        let mut tx_outs: Vec<TransactionOut> = vec![];

        // sort by highest balance
        let mut wanted_balances_vec: Vec<(AccountAddress, u64)> =
            wanted_balances.into_iter().collect();

        wanted_balances_vec.sort_by(|a, b| b.1.cmp(&a.1));

        for (account_address, balance) in wanted_balances_vec {
            tx_outs.push(TransactionOut {
                recipient: account_address,
                value: balance,
            });
        }

        // Constructs a single transaction with all required UTXOs
        let tx_data = TransactionData {
            version,
            kind: TransactionKind::Genesis,
            inputs: vec![], // No inputs required
            outputs: tx_outs,
        };
        let transaction = Transaction {
            data: tx_data,
            signature: StryiSignature(Box::new([0u8; 65])),
        };

        // Compute the Merkle root from the transactions
        let merkle_hash = Self::compute_merkle_root(std::slice::from_ref(&transaction));

        let empty_block_hash = BlockHash::empty();
        let header = BlockHeader {
            merkle_root_hash: merkle_hash,

            // Use provided values for the chain version
            version,
            difficulty_bits: 0, // Zero in genesis blocks

            // Use empty values for previous_block_hash, nonce, height and timestamp
            previous_block_hash: empty_block_hash,
            nonce: 0,
            height: 0,
            timestamp: 0,

            genesis_state: Some(genesis_state),
        };

        let data = BlockData {
            transactions: vec![transaction],
        };

        Self { header, data }
    }

    /// Returns `true` for the genesis block/header.
    pub fn is_genesis(&self) -> bool {
        self.header.genesis_state.is_some()
    }

    /// Recomputes the Merkle root based on current block data and updates the block header.
    /// Usually used if transactions were modified or appended after block creation.
    pub fn update_merkle_root(&mut self) {
        self.header.merkle_root_hash = Self::compute_merkle_root(&self.data.transactions);
    }

    /// Computes the Merkle root from a list of transactions using MerkleTree.
    pub fn compute_merkle_root(transactions: &[Transaction]) -> MerkleHash {
        // Convert each transaction into a byte vector by serializing it via postcard
        let leaves_data: Vec<Vec<u8>> = transactions
            .iter()
            .map(|tx| {
                postcard::to_stdvec(tx)
                    .expect("Failed to serialize transaction")
            })
            .collect();

        let tree = MerkleTree::new(&leaves_data);
        tree.root_hash().unwrap_or(MerkleHash::empty()) // handle empty block or error case
    }

    /// Calculates the block hash.
    /// If genesis, returns `Bx00000000000000000000000000000000000000`
    pub fn block_hash(&self) -> BlockHash {
        if self.is_genesis() {
            return BlockHash::empty();
        };

        let header_bytes = postcard::to_stdvec(&self.header)
            .expect("Failed to serialize block header");

        // Create the final block hash
        BlockHash::new(&header_bytes)
    }

    /// Returns the miner reward recipient from the coinbase transaction.
    /// Returns `None` for genesis or structurally invalid non-genesis blocks.
    pub fn miner_address(&self) -> Option<AccountAddress> {
        if self.is_genesis() {
            return None;
        }

        let first_tx = self.data.transactions.first()?;
        if first_tx.data.kind != TransactionKind::Coinbase {
            return None;
        }
        debug_assert_eq!(first_tx.data.kind, TransactionKind::Coinbase);

        if first_tx.data.outputs.len() != 1 {
            return None;
        }
        debug_assert_eq!(first_tx.data.outputs.len(), 1);

        first_tx.data.outputs.first().map(|output| output.recipient)
    }

    /// Validates the Proof-of-Work (PoW) for the block.
    ///
    /// Computes the hash of the block and then checks if it meets the difficulty target specified in the header's `bits` field.
    /// For genesis blocks return `true`.
    pub fn validate_proof_of_work(&self) -> bool {
        // Genesis blocks can go without PoW checks
        if self.is_genesis() {
            return true;
        }

        let hash = self.block_hash();

        meets_difficulty(&hash, self.header.difficulty_bits)
    }

    /// Validates the Merkle root of the block.
    ///
    /// Recomputes the Merkle root from the block's transactions and compares it
    /// with the `merkle_root_hash` stored in the block header.
    pub fn is_merkle_root_valid(&self) -> bool {
        let computed_root = Self::compute_merkle_root(&self.data.transactions);
        computed_root == self.header.merkle_root_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::AccountAddress;
    use crate::transactions::{TransactionData, TransactionKind};
    use k256::ecdsa::SigningKey;
    use rand::rng;

    #[test]
    fn test_create_block_and_compute_hash() {
        // Example: create a dummy transaction
        let tx_data = TransactionData {
            kind: TransactionKind::Payment,
            version: 1,
            inputs: vec![],
            outputs: vec![],
        };

        // Generate ephemeral signing key to sign transaction
        let signing_key = SigningKey::random(&mut rng());

        // Sign transaction data
        let signed_tx = tx_data.sign(&signing_key);

        // Create a block
        let prev_hash = BlockHash::empty(); // Some placeholder
        let mut block = Block::new(vec![signed_tx], prev_hash, 42, 16, 1_700_000_000, 1);

        // Initial block hash
        let hash1 = block.block_hash();

        // Add another transaction and update Merkle root
        let tx_data2 = TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![],
            outputs: vec![],
        };
        let signed_tx2 = tx_data2.sign(&signing_key);
        block.data.transactions.push(signed_tx2);
        block.update_merkle_root();

        // Check if block hash changed after adding a transaction
        let hash2 = block.block_hash();
        assert_ne!(
            hash1, hash2,
            "Block hash should change if the block's transactions changed"
        );

        // Print out the resulting block hashes
        println!("Block hash1: {}", hash1);
        println!("Block hash2: {}", hash2);
    }

    #[test]
    fn miner_address_returns_coinbase_recipient() {
        let miner = AccountAddress::new(&[7u8; 20]);
        let coinbase = Transaction::new_unsigned(TransactionData {
            version: 1,
            kind: TransactionKind::Coinbase,
            inputs: vec![],
            outputs: vec![TransactionOut {
                value: 50,
                recipient: miner,
            }],
        });

        let block = Block::new(vec![coinbase], BlockHash::empty(), 1, 8, 1_700_000_000, 1);

        assert_eq!(block.miner_address(), Some(miner));
    }

    #[test]
    fn miner_address_returns_none_for_non_coinbase_first_transaction() {
        let tx = Transaction::new_unsigned(TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs: vec![],
            outputs: vec![],
        });

        let block = Block::new(vec![tx], BlockHash::empty(), 1, 8, 1_700_000_000, 1);

        assert_eq!(block.miner_address(), None);
    }
}
