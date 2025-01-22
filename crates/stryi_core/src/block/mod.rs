mod block_hash;
mod mining;

use std::collections::HashMap;
pub use block_hash::{BlockHash};
pub use mining::meets_difficulty;

use serde::{Deserialize, Serialize};
use crate::address::AccountAddress;
use crate::merkletree::{MerkleHash, MerkleTree};
use crate::transactions::{StryiSignature, Transaction, TransactionData, TransactionKind, TransactionOut};

/// BlockData holds a list of transactions and any extra data if needed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockData {
    /// List of transactions included in this block
    pub transactions: Vec<Transaction>,
}


/// Block header contains essential metadata for a blockchain block.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
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
    pub difficulty_bits: u8,

    /// Unix timestamp
    pub timestamp: u64,

    /// Nonce (in Bitcoin it's 32 bits so it's enough much for StryiChain)
    pub nonce: u32,
    
    /// Boolean value proves that block is the genesis in the chain 
    pub is_genesis : bool,
}



/// Block ties together BlockHeader and BlockData.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    /// The block header
    pub header: BlockHeader,

    /// The block data (transactions)
    pub data: BlockData,
}
impl Block {
    /// Creates a new block with a given list of transactions, previous block hash, height, etc.
    /// This function calculates the Merkle root from the provided transactions.
    ///
    /// Note: nonce is set to 0 by default, can be changed in mining process.
    pub fn new(transactions: Vec<Transaction>, previous_block_hash: BlockHash, height: u64, bits: u8, timestamp: u64, version: u16) -> Self {
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
            is_genesis: false,
        };

        let data = BlockData { transactions };

        Self { header, data }
    }


    /// Creates a new genesis block with a given balances in HashMap in format
    /// @AccountAddress => 10000
    ///
    /// Function converts TxOuts from this hashmap `balances`
    /// 
    /// This function calculates the Merkle root from the provided transactions.
    pub fn new_genesis(version: u16, difficulty_bits : u8, wanted_balances: HashMap<AccountAddress, u64>) -> Self {
        
        // convert balances to TxOuts
        let mut tx_outs: Vec<TransactionOut> = vec!();

        for (account_address, balance) in wanted_balances {
            tx_outs.push(TransactionOut {
                recipient: account_address,
                value: balance,
            })
        }
        
        // Constructs single transaction with all required UTXOs
        let tx_data = TransactionData {
            version,
            kind: TransactionKind::Genesis,
            inputs: vec![], // No inputs required
            outputs: tx_outs,
        };
        let transaction = Transaction {
            data: tx_data,
            signature: StryiSignature(Box::new([0u8; 65])) // Use an empty bytes as a signature
        };
        
        
        // Compute the Merkle root from the transactions
        let merkle_hash = Self::compute_merkle_root(&vec![transaction.clone()]);
        
        let empty_block_hash = BlockHash::empty();
        let header = BlockHeader { 
            
            merkle_root_hash: merkle_hash,

            // Use provided values for chain version and difficulty bits
            version,
            difficulty_bits,

            // Use empty values for previous_block_hash, nonce, height and timestamp
            previous_block_hash : empty_block_hash,
            nonce: 0,
            height : 0,
            timestamp: 0,
            
            is_genesis: true,
        };

        let data = BlockData {
            transactions: vec![transaction],
        };
        
        Self { header, data }
    }


    /// Recomputes the Merkle root based on current block data and updates the block header.
    /// Usually used if transactions were modified or appended after block creation.
    pub fn update_merkle_root(&mut self) {
        self.header.merkle_root_hash = Self::compute_merkle_root(&self.data.transactions);
    }

    /// Computes the Merkle root from a list of transactions using MerkleTree.
    pub fn compute_merkle_root(transactions: &Vec<Transaction>) -> MerkleHash {
        // Convert each transaction into a byte vector, e.g., by serializing it
        let leaves_data: Vec<Vec<u8>> = transactions
            .iter()
            .map(|tx| bincode::serde::encode_to_vec(tx, bincode::config::standard()).expect("Failed to serialize transaction"))
            .collect();

        let tree = MerkleTree::new(&leaves_data);
        tree.root_hash()
            .unwrap_or([0u8; 32]) // handle empty block or error case
    }

    /// Returns the block hash by passing BlockHeader (serialized) to BlockHashKind.
    /// Typically, we might hash only part of the header or the entire header depending on protocol rules.
    pub fn block_hash(&self) -> BlockHash {
        // Example: encode the header and then pass it to the hashing function
        let header_bytes = bincode::serde::encode_to_vec(&self.header, bincode::config::standard())
            .expect("Failed to serialize block header");

        // Create the final block hash
        BlockHash::new(&header_bytes)
    }

    /// Validates the Proof-of-Work (PoW) for the block.
    ///
    /// This method computes the hash of the block using the block's header
    /// and then checks if it meets the difficulty target specified in the header's `bits` field.
    ///
    /// # Returns
    ///
    /// * `true` if the block hash satisfies the required difficulty.
    /// * `false` otherwise.
    pub fn validate_proof_of_work(&self) -> bool {
        let hash = self.block_hash();

        meets_difficulty(&hash, self.header.difficulty_bits)
    }

    /// Validates the Merkle root of the block.
    ///
    /// This method recomputes the Merkle root from the block's transactions and compares it
    /// with the `merkle_root_hash` stored in the block header.
    ///
    /// # Returns
    pub fn validate_merkle_root(&self) -> bool {
        let computed_root = Self::compute_merkle_root(&self.data.transactions);
        computed_root == self.header.merkle_root_hash
    }
}

#[cfg(test)]
mod tests {
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::rand_core::OsRng;
    use super::*;
    use crate::transactions::{TransactionData, TransactionKind};

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
        let signing_key = SigningKey::random(&mut OsRng);

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
}
