use rayon::prelude::*;
use rand::{rng, Rng};

use crate::{
    block::Block,
    block::block_hash::{BlockHash},
};

/// Checks if the provided block hash meets the given difficulty (bits) requirement.
///
/// The `bits` parameter indicates the number of leading zero bits required.
/// For example, if bits = 16, the first 16 bits of `hash.data` must be zero.
pub fn meets_difficulty(block_hash: &BlockHash, bits: u8) -> bool {
    // Interpret bits as the number of leading zero bits in the 256-bit hash.
    // block_hash.data is [u8; 32].
    // We'll check that the first `bits` bits of that array are zero.
    let zero_bytes = (bits / 8) as usize;
    let zero_bits_in_next_byte = bits % 8;

    // Check full zero bytes
    for i in 0..zero_bytes {
        if block_hash.data[i] != 0 {
            return false;
        }
    }

    // Check the remaining bits in the next byte
    if zero_bits_in_next_byte > 0 {
        let mask = 0xFF << (8 - zero_bits_in_next_byte);
        if block_hash.data[zero_bytes] & mask != 0 {
            return false;
        }
    }

    true
}

/// Mines the given block in parallel by generating random 32-bit nonces. 
///
/// - `block` is mutable, so if a solution is found, the block's header.nonce is updated.
/// - `max_attempts` is the maximum number of random trials across all threads.
/// - Returns `true` if a solution is found (and updates the block's nonce),
///   otherwise returns `false`.
pub fn mine_block_in_parallel(block: &mut Block, max_attempts: u64) -> bool {
    let bits = block.header.difficulty_bits;

    // We use `find_any` over a parallel iterator so that if ANY thread finds a valid nonce,
    // the search stops.
    let found_nonce = (0..max_attempts)
        .into_par_iter()
        .filter_map(|_| {
            // Each iteration picks a random nonce
            let mut rng = rng();
            let candidate_nonce = rng.random::<u32>();

            // Make a local copy of the header so we don't mutate the shared block in parallel
            let mut local_header = block.header;
            local_header.nonce = candidate_nonce;

            // Serialize the header
            let header_bytes = bincode::serde::encode_to_vec(
                &local_header,
                bincode::config::standard(),
            ).expect("Failed to serialize block header");

            // Compute the hash
            let candidate_hash = BlockHash::new(&header_bytes);

            // Check if this candidate hash meets difficulty
            if meets_difficulty(&candidate_hash, bits) {
                Some(candidate_nonce)
            } else {
                None
            }
        })
        .find_any(|_nonce| true);

    // If we found a valid nonce, update the block and return true
    if let Some(nonce) = found_nonce {
        block.header.nonce = nonce;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::rand_core::OsRng;
    use rand::random;
    use crate::address::AccountAddress;
    use crate::block::{Block, BlockHash};
    use crate::block::mining::{meets_difficulty, mine_block_in_parallel};
    use crate::transactions::{OutPoint, TransactionData, TransactionHash, TransactionIn, TransactionOut};

    #[test]
    fn test_parallel_mining_small_bits() {
        // We'll create a block with very low difficulty so we can find a solution quickly in a test.

        // 1) Generate random data for the input reference (dummy)
        let random_tx_hash = TransactionHash::new(random::<[u8; 32]>().as_slice());
        let random_account_address = AccountAddress::new(random::<[u8; 20]>().as_slice());

        // 2) Build a dummy TransactionData
        let tx_data = TransactionData {
            version: 1,
            inputs: vec![TransactionIn {
                previous_output: OutPoint {
                    txid: random_tx_hash,
                    vout: 7,
                },
                sequence: 0,
            }],
            outputs: vec![TransactionOut {
                value: 9324233284,
                recipient: random_account_address,
            }],
        };

        let signing_key = SigningKey::random(&mut OsRng);
        let signed_tx = tx_data.sign(&signing_key);

        // 4) Create a Block with a very low difficulty (bits = 4)
        let mut block = {
            let mut b = Block::new(
                vec![signed_tx],
                BlockHash::empty(), // previous_block_hash
                0,                  // height
                4,                  // bits (only 4 leading zero bits)
                1_700_000_000,      // timestamp
                1                   // version
            );
            // Make sure Merkle root is correct after adding the transaction
            b.update_merkle_root();
            b
        };

        // 5) Attempt parallel mining
        let found = mine_block_in_parallel(&mut block, 500_000);
        println!("Found solution: {}", found);

        if found {
            println!("Final nonce = {}", block.header.nonce);

            // 6) Verify difficulty on the final block
            let header_bytes = bincode::serde::encode_to_vec(
                &block.header,
                bincode::config::standard(),
            )
                .unwrap();

            let block_hash = BlockHash::new(&header_bytes);
            assert!(
                meets_difficulty(&block_hash, block.header.difficulty_bits),
                "The resulting block hash does not meet difficulty"
            );

            println!("Mined block hash: {}", block_hash);
        }
    }
}
