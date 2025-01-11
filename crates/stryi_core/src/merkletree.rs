use blake3;
use serde::{Deserialize, Serialize};

/// A 32-byte hash produced by Blake3.
pub type MerkleHash = [u8; 32];

/// Computes a Blake3 hash of the given data.
fn compute_hash(data: impl AsRef<[u8]>) -> MerkleHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(data.as_ref());
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(result.as_bytes());
    hash
}

/// Represents a Merkle Tree with its levels and root.
#[derive(Debug, Clone)]
pub struct MerkleTree {
    /// All levels of the tree, with level 0 being the leaves.
    levels: Vec<Vec<MerkleHash>>,
    /// Cached root of the tree.
    root: Option<MerkleHash>,
}

/// A proof of membership for a particular leaf in the Merkle Tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    /// Sibling hashes along the path from the leaf to the root.
    pub sibling_hashes: Vec<MerkleHash>,
    /// The index of the leaf for which this proof was generated.
    pub leaf_index: usize,
}

impl MerkleTree {
    /// Create a new Merkle tree from a list of leaf data.
    ///
    /// # Arguments
    /// * `leaves_data` - A slice of byte vectors, each representing a leaf's data.
    ///
    /// # Returns
    /// A `MerkleTree` constructed from the provided leaves.
    pub fn new(leaves_data: &[Vec<u8>]) -> Self {
        let mut levels = Vec::new();

        // Compute leaf hashes
        let mut current_level: Vec<MerkleHash> = leaves_data
            .iter()
            .map(|data| compute_hash(data))
            .collect();

        // Handle edge case: empty tree
        if current_level.is_empty() {
            return MerkleTree { levels, root: None };
        }

        // Store the leaf level
        levels.push(current_level.clone());

        // Build the tree upward until we reach the root
        while current_level.len() > 1 {
            // If odd number of nodes, duplicate the last one
            if current_level.len() % 2 != 0 {
                current_level.push(*current_level.last().unwrap());
            }

            let mut next_level = Vec::with_capacity(current_level.len() / 2);
            for pair in current_level.chunks(2) {
                // Combine a pair of hashes and push to next level
                next_level.push(Self::combine_hashes(&pair[0], &pair[1]));
            }
            levels.push(next_level.clone());
            current_level = next_level;
        }

        let root = current_level.get(0).cloned();
        MerkleTree { levels, root }
    }

    /// Combines two hashes using Blake3 by concatenating their bytes and hashing the result.
    ///
    /// # Arguments
    /// * `left` - The left hash.
    /// * `right` - The right hash.
    ///
    /// # Returns
    /// A new `MerkleHash` resulting from combining the two input hashes.
    fn combine_hashes(left: &MerkleHash, right: &MerkleHash) -> MerkleHash {
        let mut hasher = blake3::Hasher::new();
        hasher.update(left);
        hasher.update(right);
        let result = hasher.finalize();
        let mut hash = [0u8; 32];
        hash.copy_from_slice(result.as_bytes());
        hash
    }

    /// Retrieve the root hash of the Merkle tree.
    pub fn root_hash(&self) -> Option<MerkleHash> {
        self.root
    }

    /// Generate a Merkle proof for the leaf at the given index.
    ///
    /// # Arguments
    /// * `leaf_index` - Index of the leaf for which to generate a proof.
    ///
    /// # Returns
    /// `Some(MerkleProof)` if proof generation is successful, otherwise `None`.
    pub fn generate_proof(&self, leaf_index: usize) -> Option<MerkleProof> {
        // Validate tree and index boundaries
        if self.levels.is_empty() || leaf_index >= self.levels[0].len() {
            return None;
        }

        let mut index = leaf_index;
        let mut sibling_hashes = Vec::new();

        // Traverse each level except the root level to gather sibling hashes
        for level in &self.levels[..self.levels.len() - 1] {
            let sibling_index = if index % 2 == 0 { index + 1 } else { index - 1 };
            // If sibling index is out-of-bound, duplicate the last element as sibling.
            let sibling = if sibling_index < level.len() {
                level[sibling_index]
            } else {
                *level.last().unwrap()
            };
            sibling_hashes.push(sibling);
            index /= 2;
        }

        Some(MerkleProof {
            sibling_hashes,
            leaf_index,
        })
    }
}

impl MerkleProof {
    /// Verify a given proof against a leaf value and an expected root hash.
    ///
    /// # Arguments
    /// * `leaf_data` - The original data of the leaf.
    /// * `expected_root` - The expected root hash to verify against.
    ///
    /// # Returns
    /// `true` if the proof is valid and corresponds to the expected root, `false` otherwise.
    pub fn verify(&self, leaf_data: &[u8], expected_root: MerkleHash) -> bool {
        let mut computed_hash = compute_hash(leaf_data);
        let mut index = self.leaf_index;

        // Reconstruct the path from leaf to root using sibling hashes
        for sibling_hash in &self.sibling_hashes {
            computed_hash = if index % 2 == 0 {
                MerkleTree::combine_hashes(&computed_hash, sibling_hash)
            } else {
                MerkleTree::combine_hashes(sibling_hash, &computed_hash)
            };
            index /= 2;
        }

        computed_hash == expected_root
    }
}

#[cfg(test)]
mod tests {
    use bincode::config::standard;
    use rand::{rng, Rng};
    use super::*;

    #[test]
    fn test_tree_creation_and_root() {
        let transactions = vec![
            b"tx1".to_vec(),
            b"tx2".to_vec(),
            b"tx3".to_vec(),
            b"tx4".to_vec(),
        ];
        let tree = MerkleTree::new(&transactions);
        assert!(tree.root_hash().is_some(), "Root hash should exist for non-empty tree");
    }

    #[test]
    fn test_generate_and_verify_proof_valid() {
        let transactions = vec![
            b"transaction1".to_vec(),
            b"transaction2".to_vec(),
            b"transaction3".to_vec(),
            b"transaction4".to_vec(),
        ];
        let tree = MerkleTree::new(&transactions);
        let root = tree.root_hash().expect("Tree should have a root hash");

        let leaf_index = 2;
        let proof = tree.generate_proof(leaf_index).expect("Proof should be generated");
        let leaf_data = &transactions[leaf_index];

        assert!(proof.verify(leaf_data, root), "Proof should be valid for correct data");
    }

    #[test]
    fn test_verify_proof_invalid_leaf_data() {
        let transactions = vec![
            b"tx1".to_vec(),
            b"tx2".to_vec(),
            b"tx3".to_vec(),
            b"tx4".to_vec(),
        ];
        let tree = MerkleTree::new(&transactions);
        let root = tree.root_hash().expect("Tree should have a root hash");

        let leaf_index = 1;
        let proof = tree.generate_proof(leaf_index).expect("Proof should be generated");

        let incorrect_leaf_data = b"invalid".to_vec();
        assert!(
            !proof.verify(&incorrect_leaf_data, root),
            "Proof verification should fail for incorrect leaf data"
        );
    }

    #[test]
    fn test_generate_proof_out_of_bounds() {
        let transactions = vec![
            b"tx1".to_vec(),
            b"tx2".to_vec(),
        ];
        let tree = MerkleTree::new(&transactions);

        assert!(
            tree.generate_proof(5).is_none(),
            "Proof generation should return None for out-of-bounds index"
        );
    }

    #[test]
    fn test_empty_tree_no_proof() {
        let transactions: Vec<Vec<u8>> = vec![];
        let tree = MerkleTree::new(&transactions);

        assert!(
            tree.generate_proof(0).is_none(),
            "Proof should not be generated for an empty tree"
        );
    }

    #[test]
    fn test_proof_verification_wrong_root() {
        let transactions = vec![
            b"tx1".to_vec(),
            b"tx2".to_vec(),
            b"tx3".to_vec(),
            b"tx4".to_vec(),
        ];
        let tree = MerkleTree::new(&transactions);
        let leaf_index = 2;
        let proof = tree.generate_proof(leaf_index).expect("Proof should be generated");
        let leaf_data = &transactions[leaf_index];

        let fake_root = [0u8; 32];
        assert!(
            !proof.verify(leaf_data, fake_root),
            "Proof verification should fail with incorrect root hash"
        );
    }




    #[test]
    fn test_random_trees_and_proofs() {
        let mut rng = rng();

        // Generate 30 differently sized trees
        for _ in 0..30 {
            // Randomly choose the number of leaves for this tree, between 1 and 100.
            let leaf_count: usize = rng.random_range(1..=100);

            // Generate random data for each leaf
            let leaves_data: Vec<Vec<u8>> = (0..leaf_count)
                .map(|_| {
                    // Each leaf will contain a random length of bytes between 1 and 256.
                    let data_len: usize = rng.random_range(1..=256);
                    (0..data_len).map(|_| rng.random()).collect()
                })
                .collect();

            // Create a Merkle tree from the random leaves
            let tree = MerkleTree::new(&leaves_data);
            let root = tree.root_hash();

            // If the tree is empty, skip to next iteration
            if root.is_none() {
                continue;
            }
            let root = root.unwrap();

            // Select three random leaf indices to generate proofs for.
            for _ in 0..3 {
                let leaf_index = rng.random_range(0..leaf_count);

                // Generate and verify proof for the selected leaf
                if let Some(proof) = tree.generate_proof(leaf_index) {
                    let leaf_data = &leaves_data[leaf_index];

                    // Verify the correctness of the proof
                    assert!(proof.verify(leaf_data, root),
                            "Proof should verify for leaf at index {} in a tree with {} leaves",
                            leaf_index, leaf_count);

                    // Serialize the proof using bincode
                    let serialized = bincode::serde::encode_to_vec(&proof, standard())
                        .expect("Serialization should succeed");

                    
                    // Deserialize the proof back
                    let (deserialized, _decoded_bytes): (MerkleProof, _) = bincode::serde::decode_from_slice(&serialized, standard())
                        .expect("Deserialization should succeed");

                    // Verify that the deserialized proof also verifies correctly
                    assert!(deserialized.verify(leaf_data, root),
                            "Deserialized proof should verify for leaf at index {} in a tree with {} leaves",
                            leaf_index, leaf_count);
                } else {
                    panic!("Proof generation failed for valid index {} in a tree with {} leaves",
                           leaf_index, leaf_count);
                }
            }
        }
    }
}
