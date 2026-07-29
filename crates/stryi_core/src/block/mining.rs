use crate::block::{Block, NONCE_OFFSET};
use crate::block::block_hash::BlockHash;

/// Checks if the provided block hash meets the given difficulty (bits) requirement.
///
/// The `bits` parameter indicates the number of leading zero bits required.
/// For example, if bits = 16, the first 16 bits of `hash.data` must be zero.
pub fn meets_difficulty(block_hash: &BlockHash, bits: u8) -> bool {
    // always passes
    if bits == 0 {
        return true;
    }

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

/// Mine a block by finding a valid nonce.
/// Pre-computes the header buffer once, then memcpys only the nonce per attempt.
/// `should_stop` is polled between batches.
pub fn mine_block_memcpy<F: Fn() -> bool>(block: &mut Block, should_stop: F) -> bool {
    use rayon::iter::{IntoParallelIterator, ParallelIterator};
    use rand::{rng, RngExt};

    const BATCH: u64 = 100_000;
    let bits = block.header.difficulty_bits;
    let base_buf = block.header.to_hash_bytes();

    while !should_stop() {
        let found = (0..BATCH)
            .into_par_iter()
            .filter_map(|_| {
                let candidate: u32 = rng().random();
                let mut buf = base_buf;
                buf[NONCE_OFFSET..NONCE_OFFSET + 4].copy_from_slice(&candidate.to_le_bytes());
                let hash = BlockHash::new(&buf);
                if meets_difficulty(&hash, bits) {
                    Some(candidate)
                } else {
                    None
                }
            })
            .find_any(|_| true);

        if let Some(nonce) = found {
            block.header.nonce = nonce;
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use crate::address::AccountAddress;
    use crate::block::mining::{meets_difficulty, mine_block_memcpy};
    use crate::block::{Block, BlockHash};
    use crate::transactions::{
        OutPoint, TransactionData, TransactionHash, TransactionIn, TransactionKind, TransactionOut,
    };
    use k256::ecdsa::SigningKey;
    use rand::rng;
    use rand::random;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn test_parallel_mining_small_bits() {
        // We'll create a block with very low difficulty so we can find a solution quickly in a test.

        // Generate random data for the input reference (dummy)
        let random_tx_hash = TransactionHash::new(random::<[u8; 32]>().as_slice());
        let random_account_address = AccountAddress::new(random::<[u8; 20]>().as_slice());

        // Build a dummy TransactionData
        let tx_data = TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
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

        let signing_key = SigningKey::random(&mut rng());
        let signed_tx = tx_data.sign(&signing_key);

        // Create a Block with a very low difficulty (bits = 4)
        let mut block = {
            let mut b = Block::new(
                vec![signed_tx],
                BlockHash::empty(), // previous_block_hash
                0,                  // height
                4,                  // bits (only 4 leading zero bits)
                1_700_000_000,      // timestamp
                1,                  // version
            );
            // Make sure Merkle root is correct after adding the transaction
            b.update_merkle_root();
            b
        };

        // Attempt parallel mining; cap at 5 batches (~500k attempts)
        let batches = AtomicU64::new(0);
        let found = mine_block_memcpy(&mut block, || batches.fetch_add(1, Ordering::Relaxed) >= 5);
        println!("Found solution: {}", found);

        if found {
            println!("Final nonce = {}", block.header.nonce);

            // Verify difficulty on the final block
            let block_hash = block.block_hash();
            assert!(
                meets_difficulty(&block_hash, block.header.difficulty_bits),
                "The resulting block hash does not meet difficulty"
            );

            println!("Mined block hash: {}", block_hash);
        }
    }
}
