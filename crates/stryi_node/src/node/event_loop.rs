use crate::error::StryiNodeError;
use crate::grpc::{GRPC_SERVICE_TAG, StryiSyncService, StryiSyncServiceConfig};
use crate::grpc_services::blockchain_sync_server::BlockchainSyncServer;
use crate::http::{HTTP_SERVICE_TAG, StryiHttpServiceConfig};
use crate::middleware::ready::{ReadyFlag, ReadyGateLayer};
use crate::node::ibd::ingest_ibd_batch;
use crate::node::lca::find_lca;
use crate::node::miner_bridge::MinerBridge;
use crate::node::prune_reorg_transaction_indexes;
use crate::node::remote_peer::RemotePeer;
use crate::node::sync::{connect_to_peer, query_sync_peers};
use crate::tls::NodeTlsIdentity;
use multiaddr::Multiaddr;
use std::sync::Arc;
use std::time::Duration;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::{ConsensusEngine, ConsensusVerdict, StryiConsensusEngine};
use stryi_core::mempool::MemPool;
use stryi_core::storage::StorageStats;
use stryi_network::{
    BroadcastBlock, ChainTipAnnouncement, NetworkCommand, NetworkEvent, PeerId, ServiceRecord,
    ServiceTransportSecurity, StryiNetworkError,
};
use stryi_storage::StryiStorage;
use tokio::join;
use tokio::sync::broadcast::Sender;
use tokio::sync::oneshot;
use tokio::sync::{Mutex, RwLock, Semaphore, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tonic::transport::{Server, ServerTlsConfig};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, trace, warn};

/// Main event loop of the node.
pub struct EventLoop {
    pub(crate) storage: Arc<RwLock<StryiStorage>>,
    pub(crate) mempool: Arc<RwLock<MemPool>>,
    pub(crate) consensus_engine: Arc<Mutex<StryiConsensusEngine<StryiStorage>>>,
    pub(crate) sync_service_config: StryiSyncServiceConfig,
    pub(crate) tip_updates_sender: Sender<BlockHash>,
    pub(crate) miner_bridge: Option<MinerBridge>,
    pub(crate) tls_identity: NodeTlsIdentity,
    pub(crate) http_service_config: StryiHttpServiceConfig,
    pub(crate) grpc_is_ready: ReadyFlag,
    pub(crate) http_is_ready: ReadyFlag,
    pub(crate) http_advertise_address: Multiaddr,
    pub(crate) grpc_advertise_address: Multiaddr,
    pub(crate) cmd_sender: mpsc::Sender<NetworkCommand>,
    pub(crate) net_events: broadcast::Receiver<NetworkEvent>,
    pub(crate) cancellation_token: CancellationToken,
}

impl EventLoop {
    /// Start The Main Loop Of The Node
    pub async fn run(self) -> Result<(), StryiNodeError> {
        let EventLoop {
            storage,
            mempool,
            consensus_engine,
            sync_service_config,
            tip_updates_sender,
            miner_bridge,
            tls_identity,
            http_service_config,
            grpc_is_ready,
            http_is_ready,
            http_advertise_address,
            grpc_advertise_address,
            cmd_sender: net_cmd,
            mut net_events,
            cancellation_token: cancel_token,
        } = self;

        *grpc_is_ready.write().await = true;
        *http_is_ready.write().await = true;
        info!("Node synchronized. HTTP and gRPC services are now ready.");

        let peer_id = http_service_config.peer_id;

        let mut mined_blocks_receiver = miner_bridge.map(|mb| mb.mined_blocks_receiver);

        let storage_for_http = Arc::clone(&storage);
        let storage_for_grpc = Arc::clone(&storage);
        let grpc_cert_pem = tls_identity.cert_pem.clone();
        let grpc_key_pem = tls_identity.key_pem.clone();

        let tx_broadcaster = crate::http::TxBroadcaster::new(net_cmd.clone());
        let http_cancel = cancel_token.child_token();
        let http_fut = async {
            crate::http::start_http_server(
                storage_for_http,
                http_service_config.clone(),
                mempool.clone(),
                tx_broadcaster,
                http_is_ready,
                http_cancel,
            )
            .await
        };

        let grpc_cancel = cancel_token.child_token();
        let grpc_fut = async {
            let service_impl = StryiSyncService {
                config: sync_service_config.clone(),
                storage: storage_for_grpc,
            };

            let tonic_identity =
                tonic::transport::Identity::from_pem(&grpc_cert_pem, &grpc_key_pem);
            let tls_config = ServerTlsConfig::new().identity(tonic_identity);

            let svc = BlockchainSyncServer::new(service_impl);

            // readiness gate - returns 503 until node is synced
            let readiness_layer = ReadyGateLayer::new(grpc_is_ready.clone());

            let svc = ServiceBuilder::new().layer(readiness_layer).service(svc);

            info!(
                "Starting gRPC sync service on {}",
                &sync_service_config.address
            );

            Server::builder()
                .tls_config(tls_config)?
                .layer(CompressionLayer::new())
                .layer(TraceLayer::new_for_grpc())
                .add_service(svc)
                .serve_with_shutdown(sync_service_config.address, grpc_cancel.cancelled())
                .await
        };

        {
            let grpc_record = ServiceRecord::new(
                grpc_advertise_address,
                peer_id,
                GRPC_SERVICE_TAG.to_string(),
                sync_service_config.protocol_version as u32,
                ServiceTransportSecurity::TlsServerCert {
                    cert_pem: tls_identity.cert_pem,
                },
            );

            let http_record = ServiceRecord::new(
                http_advertise_address,
                peer_id,
                HTTP_SERVICE_TAG.to_string(),
                http_service_config.api_version,
                ServiceTransportSecurity::None,
            );

            info!("Successfully signed node's http service with own keypair!");

            let register = async |service: ServiceRecord| -> Result<(), StryiNodeError> {
                let (respond_to, receive_here) =
                    oneshot::channel::<Result<(), StryiNetworkError>>();

                net_cmd
                    .send(NetworkCommand::AddService {
                        service: service.clone(),
                        respond_to,
                    })
                    .await
                    .map_err(|e| {
                        StryiNodeError::other(format!("Failed to send AddService message: {:?}", e))
                    })?;

                let _ = receive_here.await.map_err(|e| {
                    StryiNodeError::other(format!(
                        "Failed to receive AddService response from the network: {:?}",
                        e
                    ))
                })?;

                info!(
                    "Successfully registered own {} service for advertising to other peers!",
                    service.kind()
                );

                Ok(())
            };

            register(grpc_record).await?;
            register(http_record).await?;
        }

        // Publish our tip so peers learn about our chain immediately
        // it may trigger other nodes to reorganize if the local node has a better chain
        let mut last_announced_tip: Option<BlockHash> = {
            let engine = consensus_engine.lock().await;
            if let Some(ann) = build_tip_announcement(&engine) {
                info!(
                    "Publishing initial chain tip: height={}, work={}",
                    ann.height, ann.cumulative_work
                );
                let hash = ann.tip_hash;
                let (respond_to, rx) = oneshot::channel();
                let _ = net_cmd
                    .send(NetworkCommand::PublishChainTip {
                        announcement: ann,
                        respond_to,
                    })
                    .await;
                if let Ok(Err(e)) = rx.await {
                    if e.is_gossipsub_insufficient_peers() {
                        info!(
                            "Skipping initial chain tip publish: no gossipsub peers connected yet"
                        );
                    } else {
                        warn!("Failed to publish initial chain tip: {e}");
                    }
                }
                Some(hash)
            } else {
                None
            }
        };

        let sync_semaphore = Arc::new(Semaphore::new(1));
        let event_cancel = cancel_token.child_token();
        let mempool_for_events = Arc::clone(&mempool);
        let storage_for_sync = Arc::clone(&storage);
        let sync_config_for_events = sync_service_config.clone();

        let event_loop_fut = async move {
            let mut tip_heartbeat = tokio::time::interval(Duration::from_secs(30));
            // Tick one time immediately because we already published our tip
            tip_heartbeat.tick().await;

            loop {
                tokio::select! {
                    // --- periodic chain tip announcement heartbeat ---
                    _ = tip_heartbeat.tick() => {
                        let engine = consensus_engine.lock().await;
                        if let Some(ann) = build_tip_announcement(&engine)
                            && last_announced_tip.as_ref() != Some(&ann.tip_hash)
                        {
                            trace!("Tip heartbeat: height={}, work={}", ann.height, ann.cumulative_work);
                            last_announced_tip = Some(ann.tip_hash);
                            let (respond_to, rx) = oneshot::channel();
                            let _ = net_cmd.send(NetworkCommand::PublishChainTip { announcement: ann, respond_to }).await;
                            if let Ok(Err(e)) = rx.await {
                                if e.is_gossipsub_insufficient_peers() {
                                    info!("Skipping chain tip heartbeat publish: no gossipsub peers connected yet");
                                } else {
                                    warn!("Failed to publish chain tip heartbeat: {e}");
                                }
                            }
                        }
                    }

                    // --- local miner produced a block ---
                    Some(mined_block) = recv_mined_block(&mut mined_blocks_receiver) => {
                        let mined_block_miner = mined_block
                            .miner_address()
                            .expect("Locally mined blocks must contain a valid coinbase");

                        info!("=========================================");
                        info!("LOCAL MINER HAS MINED A BLOCK: ");
                        info!("Hash: {}", mined_block.block_hash());
                        info!("Miner: {}", mined_block_miner);
                        info!("Merkle Root: {}", mined_block.header.merkle_root_hash);
                        info!("Transactions: {}", mined_block.data.transactions.len());
                        info!("=========================================");

                        let mut engine = consensus_engine.lock().await;
                        match engine.on_block(mined_block.clone()).await {
                            Ok(verdict) => {
                                info!("Mined block consensus verdict: {:?}", verdict);
                                let deleted_blocks = match &verdict {
                                    ConsensusVerdict::CausedReorganization { deleted_blocks } => {
                                        Some(deleted_blocks.clone())
                                    }
                                    _ => None,
                                };

                                if matches!(verdict,ConsensusVerdict::Applied { .. } | ConsensusVerdict::CausedReorganization { .. }) {
                                    let is_reorg = matches!(verdict, ConsensusVerdict::CausedReorganization { .. });
                                    let tip_ann = if is_reorg { build_tip_announcement(&engine) } else { None };

                                    let _ = tip_updates_sender.send(mined_block.block_hash());
                                    drop(engine);

                                    if let Some(deleted_blocks) = deleted_blocks
                                        && let Err(e) = prune_reorg_transaction_indexes(&storage, &deleted_blocks).await
                                    {
                                        warn!("Failed to prune transaction index after reorg: {e}");
                                    }

                                    let mut pool = mempool_for_events.write().await;
                                    pool.update_on_block(mined_block.data.clone());
                                    drop(pool);

                                    let wrapped = BroadcastBlock::new(mined_block, peer_id);
                                    let (respond_to, rx) = oneshot::channel();
                                    let _ = net_cmd.send(NetworkCommand::PublishBlock { block: wrapped, respond_to }).await;
                                    if let Ok(Err(e)) = rx.await {
                                        warn!("Failed to publish mined block: {e}");
                                    }

                                    if let Some(ann) = tip_ann {
                                        publish_reorg_tip_announcement(&net_cmd, &mut last_announced_tip, ann).await;
                                    }


                                } else {
                                    warn!("Mined block not applied (verdict: {:?}), discarding.", verdict);
                                }
                            }
                            Err(e) => warn!("Mined block rejected by consensus: {:?}", e),
                        }
                    }

                    // --- the events from the network ---
                    result = net_events.recv() => {
                        match result {

                            // -- got new block from the chain --
                            Ok(NetworkEvent::NewBlock(broadcast_block)) => {
                                if let Some(miner_address) = broadcast_block.block.miner_address() {
                                    info!(
                                        "Received block #{} ({}) from network, origin peer {}, miner {}",
                                        broadcast_block.block.header.height,
                                        broadcast_block.block.block_hash(),
                                        broadcast_block.origin_peer_id,
                                        miner_address
                                    );
                                } else {
                                    info!(
                                        "Received block #{} ({}) from network, origin peer {}",
                                        broadcast_block.block.header.height,
                                        broadcast_block.block.block_hash(),
                                        broadcast_block.origin_peer_id
                                    );
                                }

                                let orig_origin_peer_id = broadcast_block.origin_peer_id;
                                let block = broadcast_block.block;

                                let mut engine = consensus_engine.lock().await;
                                match engine.on_block(block.clone()).await {
                                    Ok(verdict) => {
                                        info!("Consensus verdict: {:?}", verdict);
                                        let deleted_blocks = match &verdict {
                                            ConsensusVerdict::CausedReorganization { deleted_blocks } => {
                                                Some(deleted_blocks.clone())
                                            }
                                            _ => None,
                                        };

                                        let is_reorg = matches!(verdict, ConsensusVerdict::CausedReorganization { .. });

                                        if matches!(verdict,
                                            ConsensusVerdict::Applied { .. } | ConsensusVerdict::CausedReorganization { .. }
                                        ) {
                                            debug!("Block applied, updating tip and clearing mempool.");
                                            let tip_ann = if is_reorg { build_tip_announcement(&engine) } else { None };

                                            let _ = tip_updates_sender.send(block.block_hash());
                                            drop(engine);

                                            if let Some(deleted_blocks) = deleted_blocks
                                                && let Err(e) = prune_reorg_transaction_indexes(&storage, &deleted_blocks).await
                                            {
                                                warn!("Failed to prune transaction index after reorg: {e}");
                                            }

                                            let mut pool = mempool_for_events.write().await;
                                            pool.update_on_block(block.data.clone());
                                            drop(pool);

                                            if let Some(ann) = tip_ann {
                                                publish_reorg_tip_announcement(&net_cmd, &mut last_announced_tip, ann).await;
                                            }
                                        } else {
                                            drop(engine);
                                        }

                                        // Re-broadcast all NEW valid blocks to gossipsub.
                                        let should_relay = matches!(verdict,
                                            ConsensusVerdict::Applied { .. } | ConsensusVerdict::Buffered | ConsensusVerdict::CausedReorganization { .. }
                                        );
                                        if should_relay {
                                            let wrapped = BroadcastBlock::new(
                                                block,
                                                orig_origin_peer_id,
                                            );
                                            let (respond_to, rx) = oneshot::channel();
                                            let _ = net_cmd.send(NetworkCommand::PublishBlock { block: wrapped, respond_to }).await;
                                            if let Ok(Err(e)) = rx.await {
                                                warn!("Failed to re-broadcast block: {e}");
                                            }
                                        }
                                    }
                                    Err(e) => warn!("Block rejected by consensus: {:?}", e),
                                }
                            }


                            // -- new transaction --
                            Ok(NetworkEvent::NewTransaction(tx)) => {
                                debug!("Received transaction from network: {:?}", tx.data.hash());
                                let mut pool = mempool_for_events.write().await;
                                match pool.add_transaction(tx).await {
                                    Ok(()) => debug!("Transaction added to mempool"),
                                    Err(e) => debug!("Transaction rejected by mempool: {:?}", e),
                                }
                            }

                            // -- another node's new tip --
                            Ok(NetworkEvent::ChainTipAnnounced { announcement: ann, source }) => {
                                let local_work = {
                                    let engine = consensus_engine.lock().await;
                                    engine.tip().map(|(_, _, w)| w).unwrap_or(0)
                                };

                                if ann.cumulative_work <= local_work {
                                    trace!(
                                        "Remote tip (work={}) <= local (work={}), ignoring",
                                        ann.cumulative_work, local_work
                                    );
                                } else {
                                    let permit = match Arc::clone(&sync_semaphore).try_acquire_owned() {
                                        Ok(p) => p,
                                        Err(_) => {
                                            debug!("Sync already running, skipping");
                                            continue;
                                        }
                                    };

                                    info!(
                                        "Remote chain heavier (remote={}, local={}) from peer {}, starting sync",
                                        ann.cumulative_work, local_work, source
                                    );

                                    let engine_for_sync = Arc::clone(&consensus_engine);
                                    let storage_for_sync = Arc::clone(&storage_for_sync);
                                    let net_cmd_for_sync = net_cmd.clone();
                                    let tip_sender = tip_updates_sender.clone();
                                    let sync_config = sync_config_for_events.clone();

                                    tokio::task::Builder::new()
                                        .name("peer-sync")
                                        .spawn(async move {
                                            match sync_from_peer(engine_for_sync, storage_for_sync, net_cmd_for_sync, tip_sender, sync_config, source).await {
                                                Ok(()) => info!("Peer sync completed"),
                                                Err(e) => warn!("Peer sync failed: {e:?}"),
                                            }
                                            drop(permit);
                                        })
                                        .expect("failed to spawn peer-sync task");
                                }
                            }


                            Ok(NetworkEvent::PeerConnected(addr)) => {
                                info!("Peer connected: {}", addr);
                            }
                            Ok(NetworkEvent::PeerDisconnected(addr)) => {
                                info!("Peer disconnected: {}", addr);
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                warn!("Network event loop lagged, missed {} events", n);
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                info!("Network event channel closed, stopping event loop");
                                break;
                            }


                        }
                    }
                    _ = event_cancel.cancelled() => {
                        info!("Network event loop cancelled");
                        break;
                    }
                }
            }
        };

        let (grpc_res, http_res, _event_res) = join!(grpc_fut, http_fut, event_loop_fut);
        grpc_res?;
        http_res?;

        Ok(())
    }
}

async fn recv_mined_block(rx: &mut Option<mpsc::Receiver<Block>>) -> Option<Block> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn publish_reorg_tip_announcement(
    net_cmd: &mpsc::Sender<NetworkCommand>,
    last_announced_tip: &mut Option<BlockHash>,
    ann: ChainTipAnnouncement,
) {
    info!(
        "Reorg: announcing new tip height={}, work={}",
        ann.height, ann.cumulative_work
    );
    let tip_hash = ann.tip_hash;
    *last_announced_tip = Some(tip_hash);

    let (respond_to, rx) = oneshot::channel();
    let _ = net_cmd
        .send(NetworkCommand::PublishChainTip {
            announcement: ann,
            respond_to,
        })
        .await;
    if let Ok(Err(e)) = rx.await {
        if e.is_gossipsub_insufficient_peers() {
            info!("Skipping reorg chain tip publish: no gossipsub peers connected yet");
        } else {
            warn!("Failed to publish reorg chain tip: {e}");
        }
    }
}

fn build_tip_announcement(
    engine: &StryiConsensusEngine<StryiStorage>,
) -> Option<ChainTipAnnouncement> {
    engine
        .tip()
        .map(|(height, hash, work)| ChainTipAnnouncement {
            height,
            tip_hash: hash,
            cumulative_work: work,
        })
}

const SYNC_PEER_CONNECT_MAX_RETRIES: u32 = 10;
const SYNC_PEER_CONNECT_RETRY_DELAY: Duration = Duration::from_secs(1);

async fn sync_from_peer(
    consensus_engine: Arc<Mutex<StryiConsensusEngine<StryiStorage>>>,
    storage: Arc<RwLock<StryiStorage>>,
    net_cmd: mpsc::Sender<NetworkCommand>,
    tip_updates_sender: Sender<BlockHash>,
    sync_config: StryiSyncServiceConfig,
    source_peer: PeerId,
) -> Result<(), StryiNodeError> {
    // Retry until the source peer publishes its gRPC service, then fall back to another sync peer.
    let (grpc_client, actual_peer) = {
        let mut attempt = 1u32;
        loop {
            match connect_to_peer(&net_cmd, &sync_config, source_peer).await {
                Ok(client) => break (client, source_peer),
                Err(err) if attempt >= SYNC_PEER_CONNECT_MAX_RETRIES => {
                    warn!(
                        "Source peer {} unreachable after {} attempts, trying other peers",
                        source_peer, SYNC_PEER_CONNECT_MAX_RETRIES
                    );
                    let candidates = query_sync_peers(&net_cmd, &sync_config)
                        .await
                        .unwrap_or_default();

                    let mut found = None;
                    for (peer_id, _) in &candidates {
                        if *peer_id == source_peer {
                            continue;
                        }
                        if let Ok(client) = connect_to_peer(&net_cmd, &sync_config, *peer_id).await
                        {
                            info!("Fallback: connected to peer {} for sync", peer_id);
                            found = Some((client, *peer_id));
                            break;
                        }
                    }
                    match found {
                        Some(pair) => break pair,
                        None => {
                            return Err(StryiNodeError::other(format!(
                                "connect_to_peer failed after {} attempts: {} (no fallback peers)",
                                SYNC_PEER_CONNECT_MAX_RETRIES, err
                            )));
                        }
                    }
                }
                Err(err) => {
                    warn!(
                        "connect_to_peer attempt {}/{} for peer {} failed: {}. Retrying in {:?}...",
                        attempt,
                        SYNC_PEER_CONNECT_MAX_RETRIES,
                        source_peer,
                        err,
                        SYNC_PEER_CONNECT_RETRY_DELAY
                    );
                    let (respond_to, rx) = oneshot::channel();
                    let _ = net_cmd
                        .send(NetworkCommand::RefreshPeerServices {
                            peer: source_peer,
                            respond_to,
                        })
                        .await;
                    let _ = rx.await;
                    tokio::time::sleep(SYNC_PEER_CONNECT_RETRY_DELAY).await;
                    attempt += 1;
                }
            }
        }
    };
    let mut remote_peer = RemotePeer::new(actual_peer, grpc_client);

    let remote_chain_info = remote_peer.request_chain_info().await?;
    let remote_height = remote_chain_info.height;

    let (local_tip_height, local_tip_hash) = {
        let s = storage.read().await;
        s.tip()
            .await
            .map_err(|e| StryiNodeError::other(format!("tip(): {e}")))?
    };

    if local_tip_height >= remote_height {
        info!(
            "Local height ({}) >= remote ({}), nothing to sync",
            local_tip_height, remote_height
        );
        return Ok(());
    }

    // Compare the peer's canonical block at our tip height, not just block presence in storage.
    let peer_includes_local_tip = match remote_peer.request_block_by_height(local_tip_height).await
    {
        Ok(block) => block.block_hash() == local_tip_hash,
        Err(_) => false,
    };

    let sync_start_height = if peer_includes_local_tip {
        info!(
            "Peer {} includes our tip, simple sync from {} to {}",
            actual_peer,
            local_tip_height + 1,
            remote_height
        );
        local_tip_height + 1
    } else {
        info!(
            "Peer {} does NOT include our tip - chains diverged. Finding LCA...",
            actual_peer
        );
        let (lca_height, lca_hash) =
            find_lca(&storage, &mut remote_peer, remote_height, local_tip_height).await?;

        info!(
            "LCA found at height={}, hash={}. Syncing from {} to {}",
            lca_height,
            lca_hash,
            lca_height + 1,
            remote_height
        );
        lca_height + 1
    };

    let batch_size = sync_config.max_blocks_range_per_request;
    let mut current_height = sync_start_height;

    while current_height <= remote_height {
        let blocks = remote_peer
            .request_blocks_by_height_range(current_height, remote_height, batch_size as u32)
            .await?;

        if blocks.is_empty() {
            warn!(
                "Peer returned empty batch at height {}, stopping",
                current_height
            );
            break;
        }

        let fetched_count = blocks.len() as u64;

        let mut engine = consensus_engine.lock().await;
        ingest_ibd_batch(&mut *engine, &storage, blocks)
            .await
            .map_err(|e| StryiNodeError::other(format!("Sync batch failed: {e}")))?;

        if let Some((_, tip_hash, _)) = engine.tip() {
            let _ = tip_updates_sender.send(tip_hash);
        }
        drop(engine);

        current_height += fetched_count;
    }

    info!(
        "Peer sync complete, synced to height {}",
        current_height - 1
    );
    Ok(())
}
