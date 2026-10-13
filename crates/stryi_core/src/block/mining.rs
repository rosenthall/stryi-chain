use crate::block::block_hash::BlockHash;
use crate::block::{Block, NONCE_OFFSET};

/// Mine a block by finding a valid nonce.
/// Pre-computes the header buffer once, then memcpys only the nonce per attempt.
/// `should_stop` is polled between batches.
pub fn mine_block_memcpy<F: Fn() -> bool>(block: &mut Block, should_stop: F) -> bool {
    use rand::{RngExt, rng};
    use rayon::iter::{IntoParallelIterator, ParallelIterator};

    const BATCH: u64 = 100_000;
    let threshold = block.header.hash_threshold;
    let base_buf = block.header.to_hash_bytes();

    while !should_stop() {
        let found = (0..BATCH)
            .into_par_iter()
            .filter_map(|_| {
                let candidate: u32 = rng().random();
                let mut buf = base_buf;
                buf[NONCE_OFFSET..NONCE_OFFSET + 4].copy_from_slice(&candidate.to_le_bytes());
                let hash = BlockHash::new(&buf);
                if hash.meets_threshold(&threshold) {
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
    use crate::block::mining::mine_block_memcpy;
    use crate::block::{Block, BlockHash};
    use crate::difficulty::HashThreshold;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn pow_v2_miner_and_header_agree() {
        let threshold = HashThreshold::MAX >> 8;
        let mut block = Block::new(vec![], BlockHash::empty(), 7, threshold, 1_700_000_000, 1);

        // A broken nonce search must not hang the test.
        let batches = AtomicU64::new(0);
        let found = mine_block_memcpy(&mut block, || batches.fetch_add(1, Ordering::Relaxed) >= 50);
        assert!(found, "miner must find a nonce for an easy threshold");

        let hash = block.block_hash();
        assert!(
            hash.meets_threshold(&block.header.hash_threshold),
            "mined block hash must satisfy the header threshold"
        );
        assert!(block.validate_proof_of_work());
    }
}
