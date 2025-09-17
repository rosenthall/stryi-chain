use bincode::config::standard;
use bincode::serde::encode_to_vec;
use futures_util::future::BoxFuture;
use rand::{Rng, rng};
use rayon::iter::ParallelIterator;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash, BlockHeader, meets_difficulty};
use stryi_core::mempool::MemPool;
use stryi_core::merkletree::{MerkleHash, calc_merkle_root};
use stryi_core::transactions::Transaction;
use stryi_network::{NetworkCommand, NetworkEvent};
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
    /// Target number of txs before we try to mine.
    tx_threshold: usize,

    /// Block version to use for mined blocks.
    block_version: u16,

    /// Max seconds to wait even if threshold not reached.
    max_delay_secs: usize,

    /// Block reward receiver.
    reward_address: AccountAddress,
}

impl StryiMinerConfig {
    /// Creates a new miner configuration with the given parameters.
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

/// Type alias for a function that retrieves the current tip of the blockchain.
/// The idea is to not provide the whole storage instance to the miner,
/// but rather a function that returns the current tip.
pub type GetCurrentTip =
    Arc<dyn Fn() -> BoxFuture<'static, Result<(u64, BlockHash), StryiStorageError>> + Send + Sync>;

/// Miner is responsible for creating new blocks by collecting transactions from the mempool
/// and mining them.
/// If block is successfully mined, it is sent to the network.
/// What this struct does not do is:
/// - It does not validate transactions/blocks
/// - Doesn't add self-mined blocks to the storage
///   It is expected that the network layer will handle these tasks.
pub struct StryiMiner {
    /// The miner's configuration.
    cfg: StryiMinerConfig,

    /// The node's mempool, used to collect transactions for mining.
    mempool: Arc<RwLock<MemPool>>,

    /// Sender for network commands, used to send mined blocks to the network.
    net_cmd: mpsc::Sender<NetworkCommand>,

    /// Receiver for network events, used to react to new blocks or transactions.
    events_rx: broadcast::Receiver<NetworkEvent>,

    /// Function to get the current tip of the blockchain.
    get_tip: GetCurrentTip,

    /// Current mining task, if any.
    /// Contains a cancellation token and the join handle for the mining task.
    /// This allows us to cancel the mining task if needed.
    current: Option<(CancellationToken, JoinHandle<()>)>,
}

impl StryiMiner {
    /// Creates a new miner instance with the given stuff
    pub fn new(
        cfg: StryiMinerConfig,
        mempool: Arc<RwLock<MemPool>>,
        get_tip: GetCurrentTip,
        events_rx: broadcast::Receiver<NetworkEvent>,
        net_cmd: mpsc::Sender<NetworkCommand>,
    ) -> Self {
        Self {
            cfg,
            mempool,
            net_cmd,
            get_tip,
            events_rx,
            current: None,
        }
    }

    /// Spawn the miner as a detached Tokio task.
    pub fn spawn(mut self) {
        tokio::spawn(async move {
            self.event_loop().await;
        });
    }

    /// Main event loop – reacts to timer, NewBlock, NewTransaction.
    async fn event_loop(&mut self) {
        // Create a periodic timer that will trigger every `max_delay_secs` seconds.
        let secs = self.cfg.max_delay_secs as u64;
        let new_timer = || interval(Duration::from_secs(secs));
        let mut tick = new_timer();

        // flag: start mining even below threshold once the timer has fired
        let mut mine_on_timeout = false;

        // -- main loop --

        loop {
            select! {
                // periodic timer tick
                _ = tick.tick() => {
                    // reached max_delay_secs so we can try to mine even if tx threshold is not reached
                    mine_on_timeout = true;

                    // try to start a new mining round
                    self.maybe_start_new_round(mine_on_timeout).await;
                }

                // handle network events
                Ok(event) = self.events_rx.recv() => match event {

                    // NewBlock event – we received a new block from the network, abort current mining attempt
                    NetworkEvent::NewBlock(_) => {
                        info!("Miner received a new block event, aborting current mining attempt.");
                        self.abort_current_attempt().await;
                        mine_on_timeout = false;

                        // reset the timer
                        tick = new_timer();


                        // wait for next tick or tx-influx before restarting
                    }

                    // NewTransaction event – we received a new transaction, check if we can do any better block
                    // The validation of the transaction is done by the mempool, so we just check if we can start a new mining round
                    NetworkEvent::NewTransaction(_) => {
                        info!("Miner received a new transaction event, checking if we can include it in the current mining attempt.");
                        if self.mempool.read().await.transaction_count() >= self.cfg.tx_threshold {

                            // kill current mining attempt if it is running
                            self.abort_current_attempt().await;

                            // if we have enough transactions, try to start a new mining round
                            self.maybe_start_new_round(false).await;
                        }
                    }

                    // ignore other events
                    _ => {}
                },
            }
        }
    }

    /// Abort current PoW task, if running.
    async fn abort_current_attempt(&mut self) {
        if let Some((token, handle)) = self.current.take() {
            token.cancel();
            drop(handle)
        }
    }

    /// Build a candidate block from the best transactions in the mempool.
    async fn build_candidate_block(
        &self,
        best_txs: Vec<Transaction>,
    ) -> Result<Block, StryiStorageError> {
        // Get the latest block hash from storage
        let get_tip = self.get_tip.clone();
        let (latest_block_height, latest_block_hash) = get_tip().await?;

        // Get current timestamp
        let system_time = SystemTime::now();
        let timestamp = system_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("SystemTime before UNIX_EPOCH, this should never normally happen")
            .as_secs();

        let mut header = BlockHeader {
            // -- dynamic values --
            previous_block_hash: latest_block_hash,
            height: latest_block_height + 1, // increment height by 1
            difficulty_bits: 0, // TODO: set proper difficulty bits based on network conditions / consensus rules
            timestamp,

            // -- static values --
            merkle_root_hash: MerkleHash::empty(), // placeholder, will be computed later
            version: self.cfg.block_version,
            nonce: 0,
            genesis_state: None,
        };

        let merkle_root = calc_merkle_root(&best_txs);
        header.merkle_root_hash = merkle_root;

        trace!("Current candidate block header is set to: {:?}", header);

        let data = stryi_core::block::BlockData {
            transactions: best_txs,
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
        if !ignore_threshold && txs_count == 0 {
            info!("No transactions in the mempool, not starting a new mining round.");
            return;
        }

        if ignore_threshold {
            info!("Starting a new mining round, ignoring tx threshold. 'Can't wait any longer!'");
        } else {
            info!(
                "Starting a new mining round, tx threshold reached: {}",
                self.cfg.tx_threshold
            );
        }

        // snapshot best transactions
        let best_txs = {
            let mp = self.mempool.read().await;
            match mp.get_best_transactions(10_000).await {
                Ok(v) if !v.is_empty() => v,
                _ => return,
            }
        };

        // build a block base
        let block_base = self
            .build_candidate_block(best_txs)
            .await
            .expect("Failed to build candidate block"); // todo: Handle candidate block build errors gracefully

        // spawn cancellable PoW task

        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let mut block_for_task = block_base.clone();
        let net_cmd = self.net_cmd.clone();
        let miner_addr = self.cfg.reward_address;

        let handle: JoinHandle<()> = tokio::task::spawn_blocking(move || {
            if mine_block(&mut block_for_task, &task_cancel) {
                // Successful mining: wrap & ship the block
                let wrapped = stryi_network::BroadcastBlock::new(
                    block_for_task,
                    miner_addr,
                    SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                );

                // We are in a blocking thread – use a tokio runtime handle
                // to send the command back into the async world.
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    rt.block_on(async move {
                        let _ = net_cmd.send(NetworkCommand::PublishBlock(wrapped)).await;
                    });
                }
            }
        });

        self.current = Some((cancel, handle));
    }
}

/// Mine a block by finding a valid nonce.
/// The function runs an infinite outer loop; every iteration launches a
/// parallel search over a **fixed** batch (1 000 000 candidate nonces).
/// After each batch it checks `cancel.is_cancelled()` and exits if asked.
/// On success, it writes the winning nonce into `block.header.nonce` and
/// returns `true`; if cancelled first, returns `false`.
fn mine_block(block: &mut Block, cancel: &CancellationToken) -> bool {
    use rayon::iter::IntoParallelIterator;

    const BATCH: u64 = 1_000_000; // candidates per Rayon batch
    let bits = block.header.difficulty_bits; // current network target

    // TODO: Pre-compute block's static parts; memcpy the varying 4-byte nonce into a buffer before hashing instead of serializing the whole header each time.

    // outer loop – repeat batches until solved or cancelled
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

    /// Require 6 leading zero *bits* – trivial for CPU tests.
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
