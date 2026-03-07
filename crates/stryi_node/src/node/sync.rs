use crate::error::StryiNodeError;
use crate::grpc::{GRPC_SERVICE_TAG, StryiSyncServiceConfig};
use crate::grpc_services::ChainInfo;
use crate::grpc_services::blockchain_sync_client::BlockchainSyncClient;
use crate::node::ibd::ingest_ibd_batch;
use crate::node::remote_peer::RemotePeer;
use crate::node::{ConnectedNode, SyncedNode};
use multiaddr::{Multiaddr, Protocol};
use std::sync::Arc;
use std::time::Duration;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::{
    BlockStorage, BlockValidator, ConsensusConsts, StorageStats, StryiConsensusEngine, UtxoStorage,
};
use stryi_core::difficulty::difficulty_calculator_from_consts;
use stryi_core::transactions::{OutPoint, UTXO, UtxoProcessor};
use stryi_network::{NetworkCommand, PeerId, ServiceRecord};
use stryi_storage::{StryiStorage, extract_utxos_from_block};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::{Instant, sleep};
use tracing::{debug, info, trace, warn};

/// How many seconds try to discover peers with the gRPC sync service
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);

/// How often to poll the network for peers with gRPC sync service
const DISCOVERY_INTERVAL: Duration = Duration::from_millis(750);

impl ConnectedNode {
    /// Bootstrap mode means this node is the source of genesis, no peers to sync from.
    /// Just builds the consensus engine from what's already in storage.
    pub async fn bootstrap(self) -> Result<SyncedNode, StryiNodeError> {
        info!("Bootstrap: skipping synchronize(); This node is the source of genesis.");

        let engine = self.build_consensus_engine().await?;

        Ok(SyncedNode {
            storage: self.storage,
            mempool: self.mempool,
            net_cmd: self.net_cmd,
            net_events: self.net_events,
            consensus_engine: Arc::new(Mutex::new(engine)),
            sync_service_config: self.sync_service_config,
        })
    }

    /// Performs synchronization with random remote peer.
    pub async fn synchronize(self) -> Result<SyncedNode, StryiNodeError> {
        info!("Start synchronizing with the network...");

        // Discover a peer with gRPC sync service, retry for up to DISCOVERY_TIMEOUT
        let started = Instant::now();
        let (peer, grpc_client) = loop {
            match find_sync_peer(&self.net_cmd, &self.sync_service_config).await {
                Ok(found) => break found,

                // if got timeout, wait some time before trying again
                Err(_) if started.elapsed() < DISCOVERY_TIMEOUT => {
                    sleep(DISCOVERY_INTERVAL).await;
                }

                Err(_) => {
                    todo!("Somehow process cases when cannot connect to peer")
                }
            }
        };

        let mut remote_peer = RemotePeer::new(peer.clone(), grpc_client.clone());

        // get external peer's chain info
        info!("Requesting Chain Info from external peer so we can compare it with local one.");
        let remote_chain_info = remote_peer.request_chain_info().await?;
        debug!(external_chain_info = ?remote_chain_info);

        // validate it
        self.validate_peer_chain_info(remote_chain_info.clone())
            .await?;

        // fetch genesis from peer, compare or save locally
        self.ensure_genesis(&mut remote_peer).await?;

        /*
          ___ _   _ _ __   ___
         / __| | | | '_ \ / __|
         \__ \ |_| | | | | (__
         |___/\__, |_| |_|\___|
               __/ |
              |___/
        github.com/rosenthall :>
        */

        let mut engine = self.build_consensus_engine().await?;

        // query remaining blocks from peer
        let (local_tip_height, local_tip_hash) = {
            let s = self.storage.read().await;
            let tip = s
                .tip()
                .await
                .map_err(|e| StryiNodeError::other(format!("tip(): {e}")))?;
            (tip.0, tip.1)
        };

        trace!(local_tip_height = ?local_tip_height, local_tip_hash = ?local_tip_hash);

        // ask the peer if its chain already includes our tip
        // this is an easy case for synchronization, since it just requires download and apply all the remaining blocks
        info!("Checking if peer has our local tip included in its chain.");

        // note: I'm not sure how it gonna behave when the only common block is genesis.
        let peer_includes_local_tip = remote_peer
            .request_block_by_hash(local_tip_hash)
            .await
            .is_ok();

        // early return if not
        if !peer_includes_local_tip {
            return Err(StryiNodeError::other(
                "Node doesn't support syncing when external peer doesn't includes our local tip",
            ));
        }

        info!(
            "Success! peer {} knows block {} (which is our local tip)! Downloading the rest of the blocks..",
            peer, local_tip_hash
        );

        let external_height = remote_chain_info.height;
        let local_work = engine.tip().map(|(_, _, w)| w).unwrap_or(0);
        let remote_work = remote_chain_info.total_difficulty as u128;

        info!(
            "Local work={} vs remote work={} (heights: {} vs {})",
            local_work, remote_work, local_tip_height, external_height
        );

        // compare cumulative work.
        if local_work >= remote_work {
            info!(
                "Local chain has more work ({}) than peer ({}). Nothing to sync.",
                local_work, remote_work
            );
        } else {
            info!(
                "Local chain has less work ({}) than peer (id={}) ({}).",
                local_work,
                peer.to_string(),
                remote_work
            );
            info!("Proceeding synchronization with peer {}", peer);
            run_ibd(
                &mut engine,
                &mut remote_peer,
                local_tip_height + 1,
                external_height,
                self.sync_service_config.max_blocks_range_per_request,
            )
            .await?;
        }

        info!("IBD complete, node is now synchronized! Ready to start own services.");

        Ok(SyncedNode {
            storage: self.storage,
            mempool: self.mempool,
            net_cmd: self.net_cmd,
            net_events: self.net_events,
            consensus_engine: Arc::new(Mutex::new(engine)),
            sync_service_config: self.sync_service_config,
        })
    }

    async fn build_consensus_engine(
        &self,
    ) -> Result<StryiConsensusEngine<StryiStorage>, StryiNodeError> {
        info!("Trying to instantize StryiConsensusEngine instance");

        let consensus_constants = build_consensus_constants(&self.storage).await?;
        let difficulty_calculator =
            difficulty_calculator_from_consts::<StryiStorage>(consensus_constants);
        let block_validator =
            BlockValidator::new(consensus_constants, difficulty_calculator.clone());
        let utxo_processor = UtxoProcessor::new();

        trace!(consensus_consts = ?consensus_constants);

        let engine = StryiConsensusEngine::new(
            consensus_constants,
            block_validator,
            utxo_processor,
            self.storage.clone(),
            difficulty_calculator,
        )
        .await
        .map_err(|e| StryiNodeError::other(format!("consensus engine init failed: {e}")))?;

        engine.startup_message();
        info!("Successfully built StryiConsensusEngine instance!");

        Ok(engine)
    }

    /// Fetch genesis from the `remote_peer`, compare with the local one, or save if we don't have one yet
    // todo: How does it integrates with always_accept cli parameter?
    async fn ensure_genesis(&self, remote_peer: &mut RemotePeer) -> Result<(), StryiNodeError> {
        let peer_id = remote_peer.peer_id();
        info!("Requesting genesis block from peer {}", peer_id);
        let remote_genesis_block: Block = remote_peer.request_genesis().await?;
        trace!("Received genesis block: {:?}", remote_genesis_block);

        let maybe_local_genesis = {
            let s = self.storage.read().await;
            s.get_block_by_height(0).await.map_err(|e| {
                StryiNodeError::other(format!("failed to query storage for genesis: {e}"))
            })?
        };

        if let Some(local_genesis_block) = maybe_local_genesis {
            // genesis is already set - compare it to one we got from peer
            let external_genesis_block = remote_genesis_block.clone();

            trace!(local_genesis = ?local_genesis_block, external_genesis = ?external_genesis_block);
            assert_eq!(
                local_genesis_block, external_genesis_block,
                "different genesis blocks detected locally and in remote peer! currently unsupported"
            );
        } else {
            // Save meta first, then commit the block
            self.genesis_bootstrap
                .confirm_and_save(
                    &remote_genesis_block,
                    &self.sync_service_config.chain_name,
                    self.sync_service_config.protocol_version as u64,
                    Some(&*format!("peer {peer_id}")),
                    true,
                )
                .map_err(|e| StryiNodeError::other(format!("confirm_and_save failed: {e}")))?;

            {
                let mut s = self.storage.write().await;
                s.put_block(&remote_genesis_block).await.map_err(|e| {
                    StryiNodeError::other(format!("failed to commit genesis block: {e}"))
                })?;

                info!("Genesis block successfully stored.");

                let utxos_to_insert: Vec<(OutPoint, UTXO)> =
                    extract_utxos_from_block(&remote_genesis_block);

                s.batch_put_utxos(utxos_to_insert).await.map_err(|e| {
                    StryiNodeError::other(format!("failed to commit genesis UTXOs: {e}"))
                })?;

                info!("Genesis UTXOs successfully stored.");

                info!("OMG IT WORKED")
            }

            info!("Genesis saved in meta and committed to storage (height=0).");
        }

        Ok(())
    }

    pub(crate) async fn validate_peer_chain_info(
        &self,
        info: ChainInfo,
    ) -> Result<(u64, BlockHash), StryiNodeError> {
        // invariants checks: protocol version and chain name must match

        let local_proto: u64 = self.sync_service_config.protocol_version as u64;
        let remote_proto: u64 = info.protocol_version as u64;
        require_chain_info_eq("protocol_version", local_proto, remote_proto)?;

        let local_chain: &str = self.sync_service_config.chain_name.as_str();
        let remote_chain: &str = info.chain_name.as_str();
        require_chain_info_eq("chain_name", local_chain, remote_chain)?;

        //  Parse remote tip hash
        let remote_tip_hash =
            BlockHash::from_hash_string(&info.latest_block_hash).map_err(|e| {
                StryiNodeError::other(format!(
                    "invalid remote tip hash '{}': {}",
                    info.latest_block_hash, e
                ))
            })?;

        // if heights equal (>0), tip hashes must match.
        let (local_height, local_tip_hash) = {
            let s = self.storage.read().await;
            s.tip()
                .await
                .map_err(|e| StryiNodeError::other(format!("tip() failed: {e}")))?
        };

        if info.height == local_height && local_height > 0 && remote_tip_hash != local_tip_hash {
            return Err(StryiNodeError::chain_info_mismatch(
                "tip_hash at the same height",
                local_tip_hash.to_string(),
                remote_tip_hash.to_string(),
            ));
        }

        // warn if a peer has less work
        let local_work = self
            .storage
            .read()
            .await
            .chain_difficulty()
            .await
            .unwrap_or(0);
        let remote_work = info.total_difficulty as u128;

        if remote_work < local_work {
            warn!(
                "peer has less work: remote_work={}, local_work={}",
                remote_work, local_work
            );
        }

        info!(
            "peer meta OK: chain='{}', proto={}, remote_height={}, remote_tip={}, total_difficulty={}, last_update={}",
            info.chain_name,
            info.protocol_version,
            info.height,
            remote_tip_hash,
            info.total_difficulty,
            info.last_update_time
        );

        Ok((info.height, remote_tip_hash))
    }
}

// simple helper to validate local values against ones from peer
#[inline]
fn require_chain_info_eq<T>(field: &'static str, local: T, remote: T) -> Result<(), StryiNodeError>
where
    T: PartialEq + ToString,
{
    if local != remote {
        return Err(StryiNodeError::chain_info_mismatch(
            field,
            local.to_string(),
            remote.to_string(),
        ));
    }
    Ok(())
}

async fn query_sync_candidates(
    net_cmd: &mpsc::Sender<NetworkCommand>,
) -> Result<Vec<(PeerId, ServiceRecord)>, StryiNodeError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    net_cmd
        .send(NetworkCommand::QueryPeersWithService {
            service: GRPC_SERVICE_TAG.to_owned(),
            respond_to: tx,
        })
        .await
        .map_err(|_| StryiNodeError::other("network command channel closed"))?;

    rx.await
        .map_err(|_| StryiNodeError::other("service query response channel closed"))
}

async fn connect_grpc(
    svc: &ServiceRecord,
) -> Result<BlockchainSyncClient<tonic::transport::Channel>, StryiNodeError> {
    let uri = grpc_uri_from_multiaddr(svc.address())
        .map_err(|e| StryiNodeError::other(format!("gRPC URI error: {e}")))?;

    let channel = tonic::transport::Endpoint::from(uri)
        .connect()
        .await
        .map_err(|e| StryiNodeError::other(format!("gRPC dial error: {e}")))?;

    Ok(BlockchainSyncClient::new(channel))
}

async fn find_sync_peer(
    net_cmd: &mpsc::Sender<NetworkCommand>,
    sync_config: &StryiSyncServiceConfig,
) -> Result<(PeerId, BlockchainSyncClient<tonic::transport::Channel>), StryiNodeError> {
    let candidates = query_sync_candidates(net_cmd).await?;

    let (peer_id, svc) = candidates
        .iter()
        .find(|(_, s)| s.version() as usize == sync_config.protocol_version)
        .ok_or_else(|| StryiNodeError::other("No compatible gRPC sync peers found"))?;

    let peer_id = *peer_id;
    info!("Using gRPC sync peer {} at {}", peer_id, svc.address());

    let client = connect_grpc(svc).await?;
    Ok((peer_id, client))
}

pub(super) async fn connect_to_peer(
    net_cmd: &mpsc::Sender<NetworkCommand>,
    sync_config: &StryiSyncServiceConfig,
    peer: PeerId,
) -> Result<BlockchainSyncClient<tonic::transport::Channel>, StryiNodeError> {
    let candidates = query_sync_candidates(net_cmd).await?;

    let (_, svc) = candidates
        .iter()
        .find(|(id, s)| *id == peer && s.version() as usize == sync_config.protocol_version)
        .ok_or_else(|| {
            StryiNodeError::other(format!("Peer {peer} has no compatible gRPC sync service"))
        })?;

    info!("Connecting to gRPC sync peer {} at {}", peer, svc.address());

    connect_grpc(svc).await
}

/// Converts multiaddr to grpc uri
/// TODO: Make gRPC be tlsed again
fn grpc_uri_from_multiaddr(addr: &Multiaddr) -> Result<tonic::transport::Uri, String> {
    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;

    for p in addr.iter() {
        match p {
            Protocol::Dns4(h) => {
                host = Some(h.to_string());
            }
            Protocol::Ip4(ip) => {
                host = Some(ip.to_string());
            }
            Protocol::Tcp(p) => {
                port = Some(p);
            }
            _ => {}
        }
    }

    let host = host.ok_or("multiaddr missing host (dns4/ip4)")?;
    let port = port.ok_or("multiaddr missing tcp port")?;

    let uri_str = format!("http://{}:{}", host, port);

    uri_str
        .parse::<tonic::transport::Uri>()
        .map_err(|e| format!("invalid grpc uri '{}': {e}", uri_str))
}

async fn run_ibd(
    engine: &mut StryiConsensusEngine<StryiStorage>,
    remote_peer: &mut RemotePeer,
    from_height: u64,
    to_height: u64,
    max_batch: usize,
) -> Result<(), StryiNodeError> {
    let mut current_height = from_height;

    while current_height <= to_height {
        let downloaded_blocks = remote_peer
            .request_blocks_by_height_range(current_height, to_height, max_batch as u32)
            .await?;

        if downloaded_blocks.is_empty() {
            warn!(
                "Peer returned empty batch at height {}. Stopping IBD.",
                current_height
            );
            break;
        }

        let fetched_count = downloaded_blocks.len() as u64;

        ingest_ibd_batch(engine, downloaded_blocks)
            .await
            .map_err(|e| {
                StryiNodeError::other(format!("Got critical error during IBD process : {e}"))
            })?;

        current_height += fetched_count;
    }

    Ok(())
}

/// Builds ConsensusConstants instance, calculates current difficulty from tip, other stuff from config.
// TODO: Refactor `build_consensus_rules` method
pub async fn build_consensus_constants(
    storage: &Arc<RwLock<StryiStorage>>,
) -> Result<ConsensusConsts, StryiNodeError> {
    // Ensure genesis exists and read it
    let genesis = {
        let db = storage.read().await;
        db.get_block_by_height(0)
            .await
            .map_err(|e| StryiNodeError::other(format!("get_block_by_height(0): {e}")))?
    };

    if let Some(g) = &genesis {
        if !g.header.is_genesis() {
            return Err(StryiNodeError::other(
                "block at height 0 is not marked as genesis",
            ));
        }

        // unwrap is safe here, because we checked for None above
        Ok(g.header.genesis_state.unwrap().consensus_consts)
    } else {
        Err(StryiNodeError::other("genesis block not found in storage"))
    }
}
