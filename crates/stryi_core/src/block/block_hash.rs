use blake3;
use hashx::{HashX, Error};

use crate::hash::HashKind;

/// Represents a specific hash kind for block hashes using HashX.
///
/// # Overview
///
/// This implementation combines HashX (an ASIC-resistant function) with BLAKE3.
/// 1. We take the input data and compute an initial BLAKE3 digest.
/// 2. That digest (32 bytes) serves as an initial seed for building a `HashX` program.
/// 3. If the seed is "weak" (`Error::ProgramConstraints`), we keep re-hashing the seed with BLAKE3
///    until we get a "strong" seed suitable for HashX.
/// 4. We process the data in 8-byte chunks, feeding each `u64` through the HashX program
///    and update a BLAKE3 hasher with the 32-byte partial output.
/// 5. Finally, we return the 32-byte BLAKE3 digest as the hash.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub struct BlockHashKind;

impl HashKind for BlockHashKind {
    const SIZE: usize = 32; // 32 bytes
    const PREFIX: &'static str = "Bx";

    /// Computes a 32-byte hash of the provided `data` using HashX in conjunction with BLAKE3.
    ///
    /// # Steps
    /// 1. Derive a `seed` by calling `blake3::hash(data)`.
    /// 2. Attempt to build a HashX program with the seed. If `Error::ProgramConstraints` occurs,
    ///    re-hash the seed with BLAKE3 and try again until successful.
    /// 3. Split the original `data` into 8-byte chunks. For each `u64` chunk, compute `hash_to_bytes`,
    ///    and feed that 32-byte output into a BLAKE3 hasher (`blender.update`).
    /// 4. If there's a leftover partial chunk (< 8 bytes), handle it by calling `hash_to_bytes`
    ///    on the remaining bytes.
    /// 5. Finalize the BLAKE3 hasher and return the resulting 32-byte digest.
    ///
    /// # Panics
    /// This method will panic if any error other than `Error::ProgramConstraints` is encountered,
    /// which theoretically should not happen in normal usage.
    fn hash(data: &[u8]) -> [u8; Self::SIZE] {
        // Generate seed for hashx and construct it
        let hashx = {
            // Compute an initial seed from BLAKE3(data)
            let mut seed = blake3::hash(data).as_bytes().to_vec();

            loop {
                match HashX::new(&seed) {
                    Ok(hx) => break hx, // Successfully built -> exit loop

                    Err(Error::ProgramConstraints) => {
                        // Re-hash the current seed if it's "weak" for HashX
                        seed = blake3::hash(&seed).as_bytes().to_vec();
                    }

                    // We assume no other error occurs in normal usage
                    Err(_) => panic!("Unexpected error while creating HashX."),
                }
            }
        };

        // accumulate hashx outputs in a BLAKE3 hasher
        let mut blender = blake3::Hasher::new();

        // Process the data in 8-byte chunks, passing partial outputs to the BLAKE3 hasher
        let mut input_u64 = 0u64;
        let mut byte_count = 0;

        for &byte in data.iter() {
            input_u64 |= (byte as u64) << (8 * (byte_count % 8));
            byte_count += 1;

            if byte_count % 8 == 0 {
                let chunk_hash = hashx.hash_to_bytes(input_u64); // [u8; 32] 
                blender.update(&chunk_hash);
                input_u64 = 0;
            }
        }

        // Handle leftover bytes (if data isn't a multiple of 8)
        if byte_count % 8 != 0 {
            let chunk_hash = hashx.hash_to_bytes(input_u64);
            blender.update(&chunk_hash);
        }

        // Finally, get the 32-byte result from BLAKE3
        let final_bytes = blender.finalize();
        let mut hash_output = [0u8; 32];
        hash_output.copy_from_slice(&final_bytes.as_bytes()[..32]);

        hash_output
    }
}

/// Type represents a hash of the block
pub type BlockHash = crate::hash::Hash<BlockHashKind>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests `BlockHashKind::hash` logic by hashing some sample inputs.
    #[test]
    fn test_block_hash_basic() {
        let input1 = b"hello world";
        let input2 = b"foo bar baz";

        // Compute two different hashes
        let hash1 = BlockHashKind::hash(input1);
        let hash2 = BlockHashKind::hash(input2);

        // We don't strictly test for "collisions" here, but we can assert they're not identical
        assert_ne!(hash1, hash2, "Different inputs should produce different block hashes.");

        // Just confirm we get 32-byte outputs
        assert_eq!(hash1.len(), 32);
        assert_eq!(hash2.len(), 32);
        
        
        // Check BlockHash TryFrom
        println!("{:?}", BlockHash::try_from(hash1.as_slice()).unwrap().to_string());
        println!("{:?}", BlockHash::try_from(hash2.as_slice()).unwrap().to_string());
    }
    
}
