mod backend;
pub use backend::MinerBackend;
pub use backend::NodeMinerBackend;

use bincode::config::standard;
use bincode::serde::encode_to_vec;
use rand::{Rng, rng};
use rayon::iter::ParallelIterator;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash, BlockHeader, meets_difficulty};
use stryi_core::mempool::MemPool;
use stryi_core::merkletree::{MerkleHash, calc_merkle_root};
use stryi_core::transactions::{Transaction, TransactionData, TransactionKind, TransactionOut};
use stryi_storage::StryiStorageError;
use tokio::select;
use tokio::sync::{RwLock, broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace};

/// Mining parameters.
#[derive(Clone)]
pub struct StryiMinerConfig {
    /// Target number of the txs before we try to mine.
    tx_threshold: usize,

    /// Block version to use for mined blocks.
    block_version: u16,

    /// Max seconds to wait even if a threshold not reached.
    max_delay_secs: usize,

    /// Block reward receiver.
    reward_address: AccountAddress,
}

impl StryiMinerConfig {
    pub fn new(
        tx_threshold: usize,
        block_version: u16,
        max_delay_secs: usize,
        reward_address: AccountAddress,
    ) -> Self {
        Self {
            tx_threshold,
            block_version,
            max_delay_secs,
            reward_address,
        }
    }
}

/// Miner is responsible for creating new blocks by collecting transactions from the mempool,
/// mining them, validating the result locally via the consensus engine, and broadcasting valid blocks.
pub struct StryiMiner {
    /// The miner's configuration.
    cfg: StryiMinerConfig,

    /// The node's mempool, used to collect transactions for mining.
    mempool: Arc<RwLock<MemPool>>,

    /// Closure-based dependencies for storage, consensus, and difficulty.
    backend: Arc<dyn MinerBackend>,

    /// Channel to send accepted mined blocks to the node for broadcasting & cleanup.
    local_blocks_tx: mpsc::Sender<Block>,

    /// Channel for receiving notifications about canonical block updates.
    tip_updates: broadcast::Receiver<BlockHash>,

    /// Current mining task, if any.
    /// Contains a cancellation token and the join handle for the mining task.
    /// This allows us to cancel the mining task if needed.
    current: Option<(CancellationToken, JoinHandle<()>)>,

    /// Cancellation token for graceful shutdown of the miner event loop.
    cancel: CancellationToken,
}

impl StryiMiner {
    pub fn new(
        cfg: StryiMinerConfig,
        mempool: Arc<RwLock<MemPool>>,
        backend: Arc<dyn MinerBackend>,
        local_blocks_tx: mpsc::Sender<Block>,
        cancel: CancellationToken,
    ) -> Self {
        let tip_updates = backend.subscribe_tip_changes();

        Self {
            cfg,
            mempool,
            backend,
            local_blocks_tx,
            tip_updates,
            current: None,
            cancel,
        }
    }

    pub fn spawn(mut self) {
        tokio::spawn(async move {
            self.event_loop().await;
        });
    }

    /// the main event loop, reacts to timer ticks an tip changes.
    async fn event_loop(&mut self) {
        let secs = self.cfg.max_delay_secs as u64;
        let new_timer = || interval(Duration::from_secs(secs));
        let mut tick = new_timer();

        loop {
            select! {
                // periodic timer ticked
                _ = tick.tick() => {
                    self.maybe_start_new_round(true).await;
                }

                // canonical tip changed
                result = self.tip_updates.recv() => {
                    match result {
                        Ok(_new_tip) => {
                            if self.current.is_some() {
                                info!("Canonical tip updated. Aborting current mining round.");
                                self.abort_current_attempt().await;
                            }
                            // start a fresh countdown from the new tip
                            tick = new_timer();
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            info!("Miner tip updates lagged by {n} messages.");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("Tip updates channel closed, stopping miner.");
                            self.abort_current_attempt().await;
                            break;
                        }
                    }
                }

                // graceful shutdown
                _ = self.cancel.cancelled() => {
                    info!("Miner received cancellation signal, stopping.");
                    self.abort_current_attempt().await;
                    break;
                }
            }
        }
    }

    /// abort the current PoW task, if running.
    async fn abort_current_attempt(&mut self) {
        if let Some((token, handle)) = self.current.take() {
            token.cancel();
            drop(handle)
        }
    }

    /// build a candidate block from the best transactions in the mempool.
    async fn build_candidate_block(
        &self,
        best_txs: Vec<Transaction>,
    ) -> Result<Block, StryiStorageError> {
        // Get the latest block hash from storage
        let (latest_block_height, latest_block_hash) = self.backend.tip().await?;

        // Get current timestamp
        let system_time = SystemTime::now();
        let timestamp = system_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("SystemTime before UNIX_EPOCH, this should never normally happen")
            .as_secs();

        // Calculate difficulty for the next block height
        let next_height = latest_block_height + 1;
        let difficulty_bits = self.backend.difficulty_bits(next_height);

        let mut header = BlockHeader {
            // -- dynamic values --
            previous_block_hash: latest_block_hash,
            height: next_height,
            difficulty_bits,
            timestamp,

            // -- static values --
            merkle_root_hash: MerkleHash::empty(), // tmp
            version: self.cfg.block_version,
            nonce: 0,
            genesis_state: None,
        };

        // Create coinbase transaction (must be first tx in block)
        let subsidy = self.backend.block_subsidy(next_height);
        let coinbase_tx = Transaction::new_unsigned(TransactionData {
            version: self.cfg.block_version,
            kind: TransactionKind::Coinbase,
            inputs: vec![],
            outputs: vec![TransactionOut {
                value: subsidy,
                recipient: self.cfg.reward_address,
            }],
        });

        // Prepend coinbase to mempool transactions
        let mut all_txs = Vec::with_capacity(1 + best_txs.len());
        all_txs.push(coinbase_tx);
        all_txs.extend(best_txs);

        // Compute merkle root over all transactions (including coinbase)
        let merkle_root = calc_merkle_root(&all_txs);
        header.merkle_root_hash = merkle_root;

        trace!("Current candidate block header is set to: {:?}", header);

        let data = stryi_core::block::BlockData {
            transactions: all_txs,
        };

        Ok(Block { header, data })
    }

    /// Try to start a new mining round (called on tick or tx-influx).
    /// `ignore_threshold` is used to ignore the tx threshold and mine anyway if there is at least one transaction.
    async fn maybe_start_new_round(&mut self, ignore_threshold: bool) {
        // If we are already mining a block, skip this round
        if self.current.is_some() {
            debug!("Miner is already mining a block, skipping this round.");
            return;
        }

        let txs_count = self.mempool.read().await.transaction_count();

        // do not start if mempool is still below threshold and ignore_threshold is false
        if txs_count < self.cfg.tx_threshold && !ignore_threshold {
            return;
        }

        // Check if there is at least one transaction in the mempool in case if `ignore_threshold` is true
        if ignore_threshold && txs_count == 0 {
            trace!("Skipping mining round: mempool is empty.");
            return;
        }

        if ignore_threshold {
            info!(
                "Starting a new mining round after max delay with {txs_count} pending transaction(s)."
            );
        } else {
            info!(
                "Starting a new mining round with {txs_count} pending transaction(s); threshold {} reached.",
                self.cfg.tx_threshold
            );
        }

        let best_txs = {
            let mp = self.mempool.read().await;
            let best = mp.get_best_transactions(10_000);
            if best.is_empty() {
                return;
            }
            best
        };

        // build a block base
        let block_base = match self.build_candidate_block(best_txs).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Failed to build candidate block: {e}. Skipping this round.");
                return;
            }
        };

        // spawn a cancellable PoW task

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let mut block_for_task = block_base.clone();
        let local_blocks_tx = self.local_blocks_tx.clone();

        let handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
            if mine_block(&mut block_for_task, &task_cancel) {
                // We are in a blocking thread, so we have to use a tokio runtime handle
                // to bridge back into the async world to send the block to the node.
                let Some(rt) = tokio::runtime::Handle::try_current().ok() else {
                    return;
                };

                rt.block_on(async move {
                    let miner_address = block_for_task
                        .miner_address()
                        .expect("Locally mined blocks must contain a valid coinbase");
                    info!(
                        "Mined block #{} by {}, sending to node for validation...",
                        block_for_task.header.height, miner_address
                    );
                    let _ = local_blocks_tx.send(block_for_task).await;
                });
            }
        });

        self.current = Some((cancel, handle));
    }
}

/// Mine a block by finding a valid nonce.
/// The function runs an infinite outer loop;
/// every iteration launches a parallel search over a **fixed** batch
/// After each batch it checks `cancel.is_cancelled()` and exits if asked.
/// On success, it writes the winning nonce into `block.header.nonce` and
/// returns `true`; if cancelled first, returns `false`.
fn mine_block(block: &mut Block, cancel: &CancellationToken) -> bool {
    use rayon::iter::IntoParallelIterator;

    const BATCH: u64 = 100_000; // candidates per Rayon batch
    let bits = block.header.difficulty_bits; // current network target

    // TODO: Pre-compute block's static parts; memcpy the varying 4-byte nonce into a buffer before hashing instead of serializing the whole header each time.

    // outer loop – repeat batches until solved or canceled
    while !cancel.is_cancelled() {
        // Rayon tries the whole batch in parallel; stops the moment `find_any`
        // receives `Some(nonce)`
        let found = (0..BATCH)
            .into_par_iter()
            .filter_map(|_| {
                // independent RNG per thread
                let mut rng = rng();
                let candidate = rng.random();

                // local header copy avoids data races
                let mut hdr = block.header;
                hdr.nonce = candidate;

                // hash(header) and difficulty check
                let bytes =
                    encode_to_vec(hdr, standard()).expect("header serialization cannot fail");

                let hash = BlockHash::new(&bytes);
                if meets_difficulty(&hash, bits) {
                    Some(candidate)
                } else {
                    None
                }
            })
            .find_any(|_| true);

        // if we found a valid nonce, write it into the block and return true
        if let Some(nonce) = found {
            block.header.nonce = nonce;
            return true;
        }
    }

    false // cancelled
}

#[cfg(test)]
mod tests {
    use super::*;
    use stryi_core::block::BlockData;
    use tokio_util::sync::CancellationToken;

    /// Require 6 leading zero *bits*
    const EASY_BITS: u8 = 6;

    #[test]
    fn pow_finds_nonce_satisfying_leading_zero_bits() {
        // minimal header
        let header = BlockHeader {
            version: 1,
            previous_block_hash: BlockHash::empty(),
            height: 1,
            difficulty_bits: EASY_BITS,
            timestamp: 0,
            merkle_root_hash: MerkleHash::empty(),
            nonce: 0,
            genesis_state: None,
        };

        let mut block = Block {
            header,
            data: BlockData {
                transactions: Vec::new(),
            },
        };

        // mine the block
        let cancel = CancellationToken::new();
        let solved = mine_block(&mut block, &cancel);

        assert!(solved, "PoW should succeed for an easy target");

        // Verify the resulting nonce really meets EASY_BITS
        let bytes = encode_to_vec(block.header, standard()).unwrap();
        let hash = BlockHash::new(&bytes);

        assert!(
            meets_difficulty(&hash, EASY_BITS),
            "nonce does not satisfy {} leading zero bits",
            EASY_BITS
        );
    }
}
