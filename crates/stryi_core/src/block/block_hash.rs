use blake3;
use hashx::{Error, HashX};

use crate::hash::HashKind;

/// Represents a specific hash kind for block hashes using HashX + BLAKE3
#[derive(Default, Clone, Copy, PartialEq, Debug, Eq, Hash)]
pub struct BlockHashKind;

impl HashKind for BlockHashKind {
    const SIZE: usize = 32; // The final output is 32 bytes
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
    /// # Panic
    /// Panics on unexpected [`HashX::new`] errors other than [`Error::ProgramConstraints`].
    fn hash(data: &[u8]) -> [u8; Self::SIZE] {
        //  Compute an initial seed from BLAKE3(data)
        let mut seed = blake3::hash(data).as_bytes().to_vec();

        // Build a HashX function, re-hashing if the seed is "weak" according to hashx
        let hashx = loop {
            match HashX::new(&seed) {
                Ok(hx) => break hx, // Successfully built -> exit loop

                Err(Error::ProgramConstraints) => {
                    // Re-hash the current seed if it's "weak"
                    seed = blake3::hash(&seed).as_bytes().to_vec();
                }

                Err(_) => unreachable!("hashx compiler error"),
            }
        };

        // Process the data in 8-byte chunks, passing 32-byte partial outputs to a BLAKE3 hasher
        let mut blender = blake3::Hasher::new();
        let mut input_u64 = 0u64;
        let mut byte_count = 0;

        for &byte in data.iter() {
            input_u64 |= (byte as u64) << (8 * (byte_count % 8));
            byte_count += 1;

            if byte_count % 8 == 0 {
                let chunk_hash = hashx.hash_to_bytes(input_u64);
                blender.update(&chunk_hash);
                input_u64 = 0;
            }
        }

        // Handle leftover bytes (< 8) if any
        if byte_count % 8 != 0 {
            let chunk_hash = hashx.hash_to_bytes(input_u64);
            blender.update(&chunk_hash);
        }

        // Finalize via BLAKE3 to just 32 bytes
        let blake3_output = blender.finalize();
        let mut result = [0u8; 32];
        result.copy_from_slice(&blake3_output.as_bytes()[..32]);

        result
    }
}

/// A hash of a block.
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

    #[test]
    fn test_block_hash_basic() {
        let input1 = b"hello world";
        let input2 = b"foo bar baz";

        // Compute two different hashes
        let hash1 = BlockHashKind::hash(input1);
        let hash2 = BlockHashKind::hash(input2);

        assert_ne!(
            hash1, hash2,
            "Different inputs should produce different block hashes."
        );

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
