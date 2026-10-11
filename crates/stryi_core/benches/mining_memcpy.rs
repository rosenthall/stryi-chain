use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use std::hint::black_box;
use stryi_core::block::{BlockHash, BlockHeader, NONCE_OFFSET};
use stryi_core::difficulty::HashThreshold;
use stryi_core::merkletree::MerkleHash;

fn sample_header() -> BlockHeader {
    BlockHeader {
        version: 1,
        merkle_root_hash: MerkleHash::empty(),
        previous_block_hash: BlockHash::empty(),
        height: 42,
        // Any fixed target is fine here: the group measures hash throughput
        // and discards the comparator result.
        hash_threshold: HashThreshold::MAX >> 1,
        timestamp: 1_700_000_000,
        nonce: 0,
        genesis_state: None,
    }
}

const BATCH: u64 = 1_000;

// old (postcard per nonce) vs new (memcpy into pre-computed buffer), both with real hash
fn mining(c: &mut Criterion) {
    let mut group = c.benchmark_group("mining");
    group.throughput(Throughput::Elements(BATCH));

    group.bench_function("old_serialize", |b| {
        let header = sample_header();
        let threshold = header.hash_threshold;
        b.iter(|| {
            (0..BATCH).into_par_iter().for_each(|i| {
                let mut hdr = header;
                hdr.nonce = i as u32;
                let bytes = postcard::to_stdvec(&hdr).unwrap();
                let hash = BlockHash::new(black_box(&bytes));
                let _ = hash.meets_threshold(&threshold);
            });
        });
    });

    group.bench_function("new_memcpy", |b| {
        let header = sample_header();
        let threshold = header.hash_threshold;
        b.iter(|| {
            let base_buf = header.to_hash_bytes();
            (0..BATCH).into_par_iter().for_each(|i| {
                let mut buf = base_buf;
                buf[NONCE_OFFSET..NONCE_OFFSET + 4].copy_from_slice(&(i as u32).to_le_bytes());
                let hash = BlockHash::new(black_box(&buf));
                let _ = hash.meets_threshold(&threshold);
            });
        });
    });

    group.finish();
}

criterion_group!(benches, mining);
criterion_main!(benches);
