use blake3;
use hashx::{HashX, Error};

use argon2::{
    Argon2, Algorithm, Params, Version,
};

use crate::hash::HashKind;


const ARGON2_SALT : [u8; 16] = [9u8; 16];

/// Represents a specific hash kind for block hashes using HashX + BLAKE3,
/// and finally hardened with Argon2id.
///
/// # Overview
///
/// 1) We take the input data and compute an initial BLAKE3 digest (32 bytes).
/// 2) That digest serves as a seed for building a `HashX` program. If the seed
///    is "weak" (`Error::ProgramConstraints`), we keep re-hashing it with BLAKE3
///    until we get a "strong" seed.
/// 3) We process the original data in 8-byte chunks, feeding each `u64` through
///    the HashX program and updating a BLAKE3 hasher with the 32-byte partial output.
/// 4) We finalize BLAKE3 to get a 32-byte intermediate result.
/// 5) We feed that intermediate 32-byte result as the "password" input
///    to Argon2 (Argon2id), using custom parameters. This step adds a memory-hard
///    layer, making the hash more resistant to parallelized hardware attacks.
///    We use [9u8; 16] as salt. Salt is not actually that required in the blockchain PoW scenario.
/// 6) The final output is a 32-byte array from Argon2.
#[derive(Default, Clone, Copy, PartialEq, Debug, Eq, Hash)]
pub struct BlockHashKind;

impl HashKind for BlockHashKind {
    const SIZE: usize = 32; // Final output is 32 bytes
    const PREFIX: &'static str = "Bx";

    /// Computes a 32-byte hash of the provided `data` using:
    /// - BLAKE3 + HashX pipeline
    /// - Followed by Argon2id for memory-hard protection.
    ///
    /// # Steps
    /// 1. Derive a `seed` via `blake3::hash(data)`.
    /// 2. Build a HashX program with that seed. If `Error::ProgramConstraints`,
    ///    re-hash the seed with BLAKE3, retry indefinitely.
    /// 3. Process `data` in 8-byte chunks (`u64`) → `hashx.hash_to_bytes(u64)` → feed 32-byte
    ///    chunk-hashes into BLAKE3 (streaming).
    /// 4. Finalize BLAKE3 to get 32 bytes (`blake3_output`).
    /// 5. **Argon2id**: Use `blake3_output` as the "password" input, with a randomly generated salt.
    ///    We configure Argon2 with certain memory/time parameters. The final Argon2 output is
    ///    another 32 bytes, which we return.
    ///
    /// # Panics
    /// - Panics if `HashX::new` hits an error other than `Error::ProgramConstraints`.
    /// - Panics if Argon2 hashing fails (in normal conditions it should succeed).
    /// So *probably* it will never panic
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

                // We assume no other error occurs in normal usage
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

        // (4) Finalize BLAKE3 → 32 bytes
        let blake3_output = blender.finalize();
        let mut blake3_hashx = [0u8; 32];
        blake3_hashx.copy_from_slice(&blake3_output.as_bytes()[..32]);

        // ---------------------------------------------------------------------
        // (5) Argon2id: use the 32-byte blake3_hashx as the "password" to get
        // a memory-hard final result. We'll produce another 32 bytes.
        // ---------------------------------------------------------------------
        

        // 64 MiB of memory (65536 KiB), 2 passes, 1 lane
        let params = Params::new(
            2048, // m_cost in KiB (2 MiB)
            2,    // t_cost (iterations)
            1,     // p_cost (parallelism)
            Some(Self::SIZE)  // output length in bytes
        ).expect("Invalid Argon2 Params");

        // Create Argon2 instance for Argon2id
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        
        
        // Prepare buffer for final 32-byte output
        let mut final_output = [0u8; Self::SIZE];
        
        // Hash into final_output, if this fails, we panic because it is not supposed to happen  
        argon2.hash_password_into(&blake3_hashx, &ARGON2_SALT, &mut final_output)
            .expect("Argon2 hashing failed unexpectedly");

        // Return the final Argon2-hardened 32 bytes
        final_output
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
        BlockHash { kind: BlockHashKind, data: [0u8; BlockHashKind::SIZE] }
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
        assert_ne!(hash1, hash2, "Different inputs should produce different block hashes.");

        // Just confirm we get 32-byte outputs
        assert_eq!(hash1.len(), 32);
        assert_eq!(hash2.len(), 32);


        // Check BlockHash TryFrom
        println!("{:?}", BlockHash::try_from(hash1.as_slice()).unwrap().to_string());
        println!("{:?}", BlockHash::try_from(hash2.as_slice()).unwrap().to_string());
    }


}
