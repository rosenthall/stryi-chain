mod block_hash;
mod mining;

pub use block_hash::{BlockHash};
use serde::{Deserialize, Serialize};
use crate::hash::HashKind;
use crate::merkletree::{MerkleHash, MerkleTree};
use crate::transactions::Transaction;

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
    pub bits: u8,

    /// Unix timestamp
    pub timestamp: u64,

    /// Nonce (in Bitcoin it's 32 bits)
    pub nonce: u32,
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
            bits,
            timestamp,
            nonce: 0,
        };

        let data = BlockData { transactions };

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transactions::TransactionData;
    use secp256k1::{Secp256k1, rand::thread_rng};

    #[test]
    fn test_create_block_and_compute_hash() {
        // Example: create a dummy transaction
        let tx_data = TransactionData {
            version: 1,
            inputs: vec![],
            outputs: vec![],
        };

        // Generate ephemeral secp256k1 keypair to sign transaction
        let secp = Secp256k1::new();
        let mut rng = thread_rng();
        let (secret_key, public_key) = secp.generate_keypair(&mut rng);

        // Sign the transaction data (assuming your updated .sign() takes &Secp256k1 + &SecretKey)
        let signed_tx = tx_data.sign(&secp, &secret_key);

        // Create a block
        let prev_hash = BlockHash::empty(); // Some placeholder
        let mut block = Block::new(vec![signed_tx], prev_hash, 42, 16, 1_700_000_000, 1);

        // Initial block hash
        let hash1 = block.block_hash();

        // Add another transaction and update Merkle root
        let tx_data2 = TransactionData {
            version: 1,
            inputs: vec![],
            outputs: vec![],
        };
        let signed_tx2 = tx_data2.sign(&secp, &secret_key);
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
