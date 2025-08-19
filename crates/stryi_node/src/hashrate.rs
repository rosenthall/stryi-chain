use rayon::iter::{IntoParallelIterator, ParallelIterator};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::ThreadPoolBuilder;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use bincode::config::standard;
use stryi_core::block::{meets_difficulty, BlockHash, BlockHeader};
use stryi_core::merkletree::MerkleHash;

pub fn warm_up() {
    const SECS: u64   = 10;
    const STEPS: u64  = 40;            // progress-bar resolution
    const BATCH: u64  = 10_000;        // <= 100 ms on most CPUs
    const DIFFICULTY: u32 = 1;         // kept explicit

    // Static header template
    let header = BlockHeader {
        version: 1,
        previous_block_hash: BlockHash::empty(),
        height: 0,
        difficulty_bits: DIFFICULTY as u8,
        timestamp: 0,
        merkle_root_hash: MerkleHash::empty(),
        nonce: 0,
        is_genesis: false,
    };

    // Pretty progress bar
    let bar = ProgressBar::new(STEPS);
    bar.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {percent}% | {eta_precise}",
        )
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "), 
    );
    let tick = Duration::from_secs_f64(SECS as f64 / STEPS as f64);

    // Shared stop flag & counter
    let stop    = Arc::new(AtomicBool::new(false));
    let counter = Arc::new(AtomicU64::new(0));

    // Worker thread on a private Rayon pool
    let worker_handle = {
        let stop    = Arc::clone(&stop);
        let counter = Arc::clone(&counter);

        thread::spawn(move || {
            let pool = ThreadPoolBuilder::new().build().expect("pool");
            pool.install(|| {
                let mut nonce_base: u32 = 0;

                while !stop.load(Ordering::Acquire) {
                    (0..BATCH).into_par_iter().for_each(|i| {
                        // fast exit once stop is raised
                        if stop.load(Ordering::Relaxed) { return; }

                        let mut hdr = header;
                        hdr.nonce = nonce_base.wrapping_add(i as u32);

                        let bytes = bincode::serde::encode_to_vec(hdr, standard()).unwrap();
                        let h = BlockHash::new(&bytes);
                        let _ = meets_difficulty(&h, DIFFICULTY as u8);
                    });
                    counter.fetch_add(BATCH, Ordering::Relaxed);
                    nonce_base = nonce_base.wrapping_add(BATCH as u32);
                }
            });
        })
    };

    // Ten-second linear progress
    let start = Instant::now();
    for _ in 0..STEPS {
        thread::sleep(tick);
        bar.inc(1);
    }

    // Tell worker to stop and wait for it
    stop.store(true, Ordering::Release);
    worker_handle.join().unwrap();
    bar.finish_and_clear();

    // Final hashrate printout
    let hps = counter.load(Ordering::Relaxed) as f64 / start.elapsed().as_secs_f64();
    println!("Estimated hashrate: {}", fmt_hashrate(hps));


}

fn fmt_hashrate(hps: f64) -> String {
    const K: f64 = 1_000.0;
    const M: f64 = 1_000_000.0;
    const G: f64 = 1_000_000_000.0;
    const T: f64 = 1_000_000_000_000.0;

    match hps {
        x if x >= T => format!("{:.2} TH/s", x / T),
        x if x >= G => format!("{:.2} GH/s", x / G),
        x if x >= M => format!("{:.2} MH/s", x / M),
        x if x >= K => format!("{:.2} kH/s", x / K),
        _           => format!("{:.0} H/s",  hps),
    }
}
