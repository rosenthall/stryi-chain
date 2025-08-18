use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use futures_util::future::BoxFuture;
use tokio::select;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::{info, trace};
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash, BlockHeader};
use stryi_core::mempool::MemPool;
use stryi_core::merkletree::MerkleHash;
use stryi_core::transactions::Transaction;
use stryi_network::{NetworkCommand, NetworkEvent};
use stryi_storage::StryiStorageError;

/// Mining parameters.
#[derive(Clone)]
pub struct StryiMinerConfig {
    /// Target number of txs before we try to mine.
    pub tx_threshold: usize,
    
    /// Block version to use for mined blocks.
    pub block_version: u16,

    /// Max seconds to wait even if threshold not reached.
    pub max_delay_secs: usize,

    /// Block reward receiver.
    pub reward_address: AccountAddress,
}




/// Type alias for a function that retrieves the current tip of the blockchain.
/// The idea is to not provide the whole storage instance to the miner,
/// but rather a function that returns the current tip.
pub type GetCurrentTip = Arc<dyn Fn() -> BoxFuture<'static, Result<(u64, BlockHash), StryiStorageError>> + Send + Sync>;

/// Miner is responsible for creating new blocks by collecting transactions from the mempool
/// and mining them.
///
/// If block is successfully mined, it is sent to the network.
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
    get_tip : GetCurrentTip,
    
    /// Current mining task, if any.
    /// Contains a cancellation token and the join handle for the mining task.
    /// This allows us to cancel the mining task if needed.
    current: Option<(CancellationToken, JoinHandle<()>)>,
}



impl StryiMiner {
    /// Creates a new miner instance with the given stuff
    pub fn new(cfg: StryiMinerConfig, mempool: Arc<RwLock<MemPool>>, get_tip: GetCurrentTip, events_rx: broadcast::Receiver<NetworkEvent>, net_cmd: mpsc::Sender<NetworkCommand>) -> Self {
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
        let mut tick = interval(Duration::from_secs(self.cfg.max_delay_secs as u64));


        // -- main loop --

        loop {
            select! {
                // periodic timer tick
                _ = tick.tick() => {
                    self.maybe_start_new_round().await;
                }
                
                
                // handle network events
                Ok(event) = self.events_rx.recv() => match event {
                    
                    // NewBlock event – we received a new block from the network, abort current mining attempt
                    NetworkEvent::NewBlock(_) => {
                        info!("Miner received a new block event, aborting current mining attempt.");
                        self.abort_current_attempt().await;
                        // wait for next tick or tx-influx before restarting
                    }
                    
                    // NewTransaction event – we received a new transaction, check if we can do any better block
                    NetworkEvent::NewTransaction(_) => {
                        info!("Miner received a new transaction event, checking if we can include it in the current mining attempt.");
                        if self.mempool.read().await.transaction_count() >= self.cfg.tx_threshold {
                            self.maybe_start_new_round().await;
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
    async fn build_candidate_block(&self, best_txs: Vec<Transaction>) -> Result<Block, StryiStorageError> {
        
        
        // Get the latest block hash from storage
        let get_tip = self.get_tip.clone();
        let (latest_block_height, latest_block_hash) = get_tip()
            .await?;
            
        
        // Get current timestamp
        let system_time = SystemTime::now();
        let timestamp = system_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("SystemTime before UNIX_EPOCH, this should never normally happen")
            .as_secs();
        
        
        let header = BlockHeader {
            // -- dynamic values --
            previous_block_hash: latest_block_hash,
            height: latest_block_height + 1, // increment height by 1
            difficulty_bits: 0, // TODO: set proper difficulty bits based on network conditions / consensus rules
            timestamp,
            
            
            // -- static values --
            
            merkle_root_hash: MerkleHash::empty(), // placeholder, will be computed later
            version: self.cfg.block_version,
            nonce: 0,
            is_genesis: false,
        };

        trace!("Current candidate block header is set to: {:?}", header);
        
        
        let data = stryi_core::block::BlockData {
            transactions: best_txs,
        };
        
        

        Ok(Block {
            header,
            data,
        })
    }
    


    /// Try to start a new mining round (called on tick or tx-influx).
    async fn maybe_start_new_round(&mut self) {

        // do not start if mempool is still below threshold
        if self.mempool.read().await.transaction_count() < self.cfg.tx_threshold {
            return;
        }

        // snapshot best transactions
        let best_txs = {
            let mp = self.mempool.read().await;
            match mp.get_best_transactions(10_000).await {
                Ok(v) if !v.is_empty() => v,
                _ => return,
            }
        };

        // build candidate block
        let mut block_base = self.build_candidate_block(best_txs).await
            .expect("Failed to build candidate block"); // todo: Handle candidate block build errors gracefully

    
        
        todo!("finish the block mining process");
        
    }
}
