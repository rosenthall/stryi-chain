use blake3;
use hashx::{Error, HashX};

use crate::hash::HashKind;

/// Represents a specific hash kind for block hashes using HashX + BLAKE3
///
/// How it works:
/// 1. We take the input data and compute an initial BLAKE3 digest (32 bytes).
/// 2. That digest serves as a seed for building a `HashX` program. If the seed
///    is "weak" (`Error::ProgramConstraints`), we keep re-hashing it with BLAKE3
///    until we get a "strong" seed.
/// 3. We process the original data in 8-byte chunks, feeding each `u64` through
///    the HashX program and updating a BLAKE3 hasher with the 32-byte partial output.
/// 4. We finalize BLake3 to get a 32-byte final result.
#[derive(Default, Clone, Copy, PartialEq, Debug, Eq, Hash)]
pub struct BlockHashKind;

impl HashKind for BlockHashKind {
    const SIZE: usize = 32; // Final output is 32 bytes
    const PREFIX: &'static str = "Bx";

    /// Computes a 32-byte hash of the provided `data` using:
    /// - hashx (with a seed derived from BLAKE3(data))
    /// - BLAKE3 for final mixing
    ///
    /// # Steps
    /// 1. Derive a `seed` via `blake3::hash(data)`.
    /// 2. Build a HashX program with that seed. If `Error::ProgramConstraints`,
    ///    re-hash the seed with BLAKE3, retry indefinitely.
    /// 3. Process `data` in 8-byte chunks (`u64`) -> `hashx.hash_to_bytes(u64)` -> feed 32-byte
    ///    chunk-hashes into BLAKE3 (streaming).
    /// 4. Finalize BLAKE3 to get 32 bytes (`blake3_output`) and return it
    ///
    /// # Panics
    /// Panics on unexpected [`HashX::new`] errors other than [`Error::ProgramConstraints`].
    fn hash(data: &[u8]) -> [u8; Self::SIZE] {
        // (1) Compute an initial seed from BLAKE3(data)
        let mut seed = blake3::hash(data).as_bytes().to_vec();

        // (2) Build a HashX function, re-hashing if the seed is "weak"
        let hashx = loop {
            match HashX::new(&seed) {
                Ok(hx) => break hx, // Successfully built -> exit loop

                Err(Error::ProgramConstraints) => {
                    // Re-hash the current seed if it's "weak"
                    seed = blake3::hash(&seed).as_bytes().to_vec();
                }

                Err(_) => panic!("Unexpected error while creating HashX."),
            }
        };

        // (3) Process the data in 8-byte chunks, passing 32-byte partial outputs to a BLAKE3 hasher
        let mut blender = blake3::Hasher::new();
        let mut input_u64 = 0u64;
        let mut byte_count = 0;

        for &byte in data.iter() {
            input_u64 |= (byte as u64) << (8 * (byte_count % 8));
            byte_count += 1;

            if byte_count % 8 == 0 {
                let chunk_hash = hashx.hash_to_bytes(input_u64); // [u8; 32]
                blender.update(&chunk_hash); // Update hasher state
                input_u64 = 0;
            }
        }

        // Handle leftover bytes (< 8) if any
        if byte_count % 8 != 0 {
            let chunk_hash = hashx.hash_to_bytes(input_u64);
            blender.update(&chunk_hash);
        }

        // (4) Finalize BLAKE3 -> 32 bytes
        let blake3_output = blender.finalize();
        let mut result = [0u8; 32];
        result.copy_from_slice(&blake3_output.as_bytes()[..32]);

        // Return the finalized BLAKE3 32 bytes
        result
    }
}

/// Type represents a hash of the block.
///
/// Internally calls `BlockHashKind::hash`, then
/// stores the result in an instance of `Hash<BlockHashKind>`.
pub type BlockHash = crate::hash::Hash<BlockHashKind>;

impl BlockHash {
    /// Returns static blockhash value (Bx0000....) for genesis block.
    pub const fn empty() -> BlockHash {
        BlockHash {
            kind: BlockHashKind,
            data: [0u8; BlockHashKind::SIZE],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests BlockHashKind::hash logic by hashing some sample inputs.
    #[test]
    fn test_block_hash_basic() {
        let input1 = b"hello world";
        let input2 = b"foo bar baz";

        // Compute two different hashes
        let hash1 = BlockHashKind::hash(input1);
        let hash2 = BlockHashKind::hash(input2);

        // We don't strictly test for "collisions" here, but we can assert they're not identical
        assert_ne!(
            hash1, hash2,
            "Different inputs should produce different block hashes."
        );

        // Just confirm we get 32-byte outputs
        assert_eq!(hash1.len(), 32);
        assert_eq!(hash2.len(), 32);

        // Check BlockHash TryFrom
        println!(
            "{:?}",
            BlockHash::try_from(hash1.as_slice()).unwrap().to_string()
        );
        println!(
            "{:?}",
            BlockHash::try_from(hash2.as_slice()).unwrap().to_string()
        );
    }
}
