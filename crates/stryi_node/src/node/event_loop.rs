use crate::error::StryiNodeError;
use crate::grpc::{GRPC_SERVICE_TAG, StryiSyncService, StryiSyncServiceConfig};
use crate::grpc_services::blockchain_sync_server::BlockchainSyncServer;
use crate::http::{HTTP_SERVICE_TAG, StryiHttpServiceConfig};
use crate::middleware::ready::{ReadyFlag, ReadyGateLayer};
use crate::node::consensus_runtime::{ConsensusOutcome, ConsensusRuntime};
use crate::node::lca::find_lca;
use crate::node::miner_bridge::MinerBridge;
use crate::node::remote_peer::RemotePeer;
use crate::node::sync::{connect_to_peer, query_sync_peers};
use crate::tls::NodeTlsIdentity;
use multiaddr::Multiaddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::StryiConsensusEngine;
use stryi_core::difficulty::ChainWork;
use stryi_core::mempool::MemPool;
use stryi_core::storage::{BlockStorage, StorageReader, StorageStats};
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
    pub(crate) storage: StryiStorage,
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

        grpc_is_ready.store(true, Ordering::Release);
        http_is_ready.store(true, Ordering::Release);

        if storage.is_failed() || cancel_token.is_cancelled() {
            grpc_is_ready.store(false, Ordering::Release);
            http_is_ready.store(false, Ordering::Release);

            return Err(StryiNodeError::other(
                "node stopped before serving requests",
            ));
        }

        info!("Node synchronized. HTTP and gRPC services are now ready.");

        let peer_id = http_service_config.peer_id;

        let mut mined_blocks_receiver = miner_bridge.map(|mb| mb.mined_blocks_receiver);

        let storage_for_http = storage.clone();
        let storage_for_grpc = storage.clone();
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
                http_is_ready.clone(),
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
                    ann.height, ann.chain_work
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
        let sync_config_for_events = sync_service_config.clone();
        let runtime = ConsensusRuntime {
            engine: consensus_engine.clone(),
            storage: storage.clone(),
            mempool: mempool.clone(),
            tip_updates: tip_updates_sender,
            cancel: cancel_token.clone(),
        };

        let grpc_ready_for_events = grpc_is_ready.clone();
        let http_ready_for_events = http_is_ready.clone();
        let event_loop_fut = async move {
            let mut tip_heartbeat = tokio::time::interval(Duration::from_secs(30));

            // Tick one time immediately because we already published our tip
            tip_heartbeat.tick().await;

            let mut sync_tasks = tokio::task::JoinSet::new();

            loop {
                tokio::select! {
                    // --- periodic chain tip announcement heartbeat ---
                    _ = tip_heartbeat.tick() => {
                        let engine = consensus_engine.lock().await;

                        if let Some(ann) = build_tip_announcement(&engine)
                            && last_announced_tip.as_ref() != Some(&ann.tip_hash)
                        {
                            trace!("Tip heartbeat: height={}, work={}", ann.height, ann.chain_work);

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

                        match runtime.apply_block(mined_block.clone()).await {
                            Ok(None) => continue,
                            Ok(Some(verdict)) => {
                                log_consensus_verdict("miner", &verdict);

                                if matches!(verdict, ConsensusOutcome::Applied { .. } | ConsensusOutcome::CausedReorganization { .. }) {
                                    let tip_ann = if matches!(verdict, ConsensusOutcome::CausedReorganization { .. }) {
                                        build_tip_announcement(&*consensus_engine.lock().await)
                                    } else { None };

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

                                match runtime.apply_block(block.clone()).await {
                                    Ok(None) => continue,
                                    Ok(Some(verdict)) => {
                                        log_consensus_verdict("network", &verdict);

                                        if matches!(verdict, ConsensusOutcome::CausedReorganization { .. }) {
                                            let ann = build_tip_announcement(&*consensus_engine.lock().await);

                                            if let Some(ann) = ann {
                                                publish_reorg_tip_announcement(&net_cmd, &mut last_announced_tip, ann).await;
                                            }
                                        }

                                        // Re-broadcast all NEW valid blocks to gossipsub.
                                        let should_relay = matches!(verdict,
                                            ConsensusOutcome::Applied { .. } | ConsensusOutcome::Buffered | ConsensusOutcome::CausedReorganization { .. }
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
                                    engine.tip().map(|(_, _, w)| w).unwrap_or(ChainWork::ZERO)
                                };

                                if ann.chain_work <= local_work {
                                    trace!(
                                        "Remote tip (work={}) <= local (work={}), ignoring",
                                        ann.chain_work, local_work
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
                                        ann.chain_work, local_work, source
                                    );

                                    let runtime_for_sync = runtime.clone();
                                    let net_cmd_for_sync = net_cmd.clone();
                                    let sync_config = sync_config_for_events.clone();

                                    sync_tasks.spawn(async move {
                                        let result = sync_from_peer(runtime_for_sync, net_cmd_for_sync, sync_config, source).await;
                                        drop(permit);

                                        result
                                    });
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
                    Some(result) = sync_tasks.join_next(), if !sync_tasks.is_empty() => {
                        match result {
                            Ok(Ok(())) => info!("Peer sync completed"),
                            Ok(Err(e)) => warn!("Peer sync failed: {e}"),
                            Err(e) => {
                                runtime.storage.mark_failed();
                                runtime.cancel.cancel();

                                warn!("Peer sync task failed: {e}");
                            }
                        }
                    }
                    _ = event_cancel.cancelled() => {
                        info!("Network event loop cancelled");
                        break;
                    }
                }
            }

            runtime.cancel.cancel();
            grpc_ready_for_events.store(false, Ordering::Release);
            http_ready_for_events.store(false, Ordering::Release);

            while let Some(result) = sync_tasks.join_next().await {
                if let Err(e) = result {
                    warn!("Peer sync task failed during shutdown: {e}");
                }
            }
        };

        let (grpc_res, http_res, _event_res) = join!(grpc_fut, http_fut, event_loop_fut);
        grpc_res?;
        http_res?;

        if storage.is_failed() {
            return Err(StryiNodeError::other("storage failed; node stopped"));
        }

        Ok(())
    }
}

async fn recv_mined_block(rx: &mut Option<mpsc::Receiver<Block>>) -> Option<Block> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

fn log_consensus_verdict(source: &str, verdict: &ConsensusOutcome) {
    let (outcome, new_head, chain_work, attached, detached) = match verdict {
        ConsensusOutcome::Applied {
            new_head,
            chain_work,
        } => ("applied", new_head, chain_work, 1, 0),
        ConsensusOutcome::CausedReorganization {
            new_head,
            chain_work,
            attached,
            detached,
        } => ("reorganized", new_head, chain_work, *attached, *detached),
        _ => {
            info!(source, ?verdict, "Consensus verdict");
            return;
        }
    };

    info!(
        source,
        outcome,
        height = new_head.0,
        tip = %new_head.1,
        work = %chain_work,
        attached,
        detached,
        "Consensus verdict"
    );
}

async fn publish_reorg_tip_announcement(
    net_cmd: &mpsc::Sender<NetworkCommand>,
    last_announced_tip: &mut Option<BlockHash>,
    ann: ChainTipAnnouncement,
) {
    info!(
        "Reorg: announcing new tip height={}, work={}",
        ann.height, ann.chain_work
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
            chain_work: work,
        })
}

const SYNC_PEER_CONNECT_MAX_RETRIES: u32 = 10;
const SYNC_PEER_CONNECT_RETRY_DELAY: Duration = Duration::from_secs(1);

async fn sync_from_peer(
    runtime: ConsensusRuntime,
    net_cmd: mpsc::Sender<NetworkCommand>,
    sync_config: StryiSyncServiceConfig,
    source_peer: PeerId,
) -> Result<(), StryiNodeError> {
    let prepared = tokio::select! {
        _ = runtime.cancel.cancelled() => return Ok(()),
        result = prepare_peer_sync(&runtime, &net_cmd, &sync_config, source_peer) => result?,
    };

    let Some((mut remote_peer, remote_height, sync_start_height)) = prepared else {
        return Ok(());
    };

    let batch_size = sync_config.max_blocks_range_per_request;
    let mut current_height = sync_start_height;

    while current_height <= remote_height {
        let blocks = tokio::select! {
            _ = runtime.cancel.cancelled() => return Ok(()),
            result = remote_peer.request_blocks_by_height_range(current_height, remote_height, batch_size as u32) => result?,
        };

        if blocks.is_empty() {
            warn!(
                "Peer returned empty batch at height {}, stopping",
                current_height
            );
            break;
        }

        let fetched_count = blocks.len() as u64;

        let verdicts = runtime.apply_blocks(blocks).await?;

        for verdict in verdicts {
            if let ConsensusOutcome::Rejected(error) = verdict {
                return Err(StryiNodeError::other(format!(
                    "Sync block rejected: {error}"
                )));
            }
        }

        current_height += fetched_count;
    }

    info!(
        "Peer sync complete, synced to height {}",
        current_height - 1
    );

    Ok(())
}

async fn prepare_peer_sync(
    runtime: &ConsensusRuntime,
    net_cmd: &mpsc::Sender<NetworkCommand>,
    sync_config: &StryiSyncServiceConfig,
    source_peer: PeerId,
) -> Result<Option<(RemotePeer, u64, u64)>, StryiNodeError> {
    let Some(mut remote_peer) =
        connect_sync_peer(&runtime.cancel, net_cmd, sync_config, source_peer).await?
    else {
        return Ok(None);
    };

    let actual_peer = remote_peer.peer_id();

    let remote_chain_info = remote_peer.request_chain_info().await?;
    let remote_height = remote_chain_info.height;

    if remote_chain_info.chain_name != sync_config.chain_name
        || remote_chain_info.protocol_version as usize != sync_config.protocol_version
    {
        return Err(StryiNodeError::other(
            "peer chain metadata does not match local configuration",
        ));
    }

    let remote_genesis = remote_peer.request_genesis().await?;
    let local_genesis = runtime
        .storage
        .read()?
        .get_block_by_height(0)?
        .ok_or_else(|| StryiNodeError::other("local genesis is missing"))?;

    if remote_genesis != local_genesis {
        return Err(StryiNodeError::other(
            "peer genesis does not match the local chain",
        ));
    }

    let (local_tip_height, local_tip_hash, local_work) = {
        let s = runtime.storage.read()?;
        let (height, hash) = s.tip()?;

        (height, hash, s.chain_work()?)
    };

    if local_work >= remote_chain_info.chain_work {
        info!(
            "Local work ({}) >= remote ({}), nothing to sync",
            local_work, remote_chain_info.chain_work
        );

        return Ok(None);
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

        let (lca_height, lca_hash) = find_lca(
            &runtime.storage,
            &mut remote_peer,
            remote_height,
            local_tip_height,
        )
        .await?;

        info!(
            "LCA found at height={}, hash={}. Syncing from {} to {}",
            lca_height,
            lca_hash,
            lca_height + 1,
            remote_height
        );

        lca_height + 1
    };

    Ok(Some((remote_peer, remote_height, sync_start_height)))
}

async fn connect_sync_peer(
    cancel: &CancellationToken,
    net_cmd: &mpsc::Sender<NetworkCommand>,
    sync_config: &StryiSyncServiceConfig,
    source_peer: PeerId,
) -> Result<Option<RemotePeer>, StryiNodeError> {
    // Retry until the source peer publishes its gRPC service, then fall back to another sync peer.
    let mut attempt = 1u32;

    loop {
        let result = tokio::select! {
            _ = cancel.cancelled() => return Ok(None),
            result = connect_to_peer(net_cmd, sync_config, source_peer) => result,
        };

        match result {
            Ok(client) => return Ok(Some(RemotePeer::new(source_peer, client))),
            Err(err) if attempt >= SYNC_PEER_CONNECT_MAX_RETRIES => {
                warn!(
                    "Source peer {} unreachable after {} attempts, trying other peers",
                    source_peer, SYNC_PEER_CONNECT_MAX_RETRIES
                );

                return connect_fallback_sync_peer(net_cmd, sync_config, source_peer, err)
                    .await
                    .map(Some);
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
}

async fn connect_fallback_sync_peer(
    net_cmd: &mpsc::Sender<NetworkCommand>,
    sync_config: &StryiSyncServiceConfig,
    source_peer: PeerId,
    source_error: StryiNodeError,
) -> Result<RemotePeer, StryiNodeError> {
    let candidates = query_sync_peers(net_cmd, sync_config)
        .await
        .unwrap_or_default();

    for (peer_id, _) in &candidates {
        if *peer_id == source_peer {
            continue;
        }

        if let Ok(client) = connect_to_peer(net_cmd, sync_config, *peer_id).await {
            info!("Fallback: connected to peer {} for sync", peer_id);

            return Ok(RemotePeer::new(*peer_id, client));
        }
    }

    Err(StryiNodeError::other(format!(
        "connect_to_peer failed after {} attempts: {} (no fallback peers)",
        SYNC_PEER_CONNECT_MAX_RETRIES, source_error
    )))
}
