use crate::error::StryiNodeError;
use crate::grpc::{GRPC_SERVICE_TAG, StryiSyncServiceConfig};
use crate::grpc_services::ChainInfo;
use crate::grpc_services::blockchain_sync_client::BlockchainSyncClient;
use crate::node::ibd::ingest_ibd_batch;
use crate::node::lca::find_lca;
use crate::node::remote_peer::RemotePeer;
use crate::node::{ConnectedNode, SyncedNode};
use crate::util::extract_tls_verification_host;
use multiaddr::{Multiaddr, Protocol};
use std::sync::Arc;
use std::time::Duration;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::{
    BlockStorage, BlockValidator, ConsensusConsts, StorageStats, StryiConsensusEngine, UtxoStorage,
};
use stryi_core::difficulty::difficulty_calculator_from_consts;
use stryi_core::transactions::{OutPoint, UTXO, UtxoProcessor};
use stryi_network::{NetworkCommand, PeerId, ServiceRecord, ServiceTransportSecurity};
use stryi_storage::{StryiStorage, extract_utxos_from_block};
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::sleep;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};
use tracing::{debug, info, trace, warn};

const PEER_DISCOVERY_MAX_RETRIES: u32 = 30;
const PEER_DISCOVERY_RETRY_DELAY: Duration = Duration::from_millis(500);

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

        let (local_tip_height, local_tip_hash) = {
            let s = self.storage.read().await;
            let tip = s
                .tip()
                .await
                .map_err(|e| StryiNodeError::other(format!("tip(): {e}")))?;
            (tip.0, tip.1)
        };

        trace!(local_tip_height = ?local_tip_height, local_tip_hash = ?local_tip_hash);

        // Discover a peer with the best chain.
        // Peers may not be immediately available, so retry with a short delay
        let (mut remote_peer, remote_chain_info) = {
            let mut attempt = 1;

            loop {
                match self.find_best_peer().await {
                    Ok(peer) => break peer,
                    Err(err) if attempt >= PEER_DISCOVERY_MAX_RETRIES => {
                        return Err(StryiNodeError::other(format!(
                            "peer discovery failed after {} attempts: {}",
                            PEER_DISCOVERY_MAX_RETRIES, err
                        )));
                    }
                    Err(err) => {
                        warn!(
                            "Peer discovery attempt {}/{} failed: {}. Retrying in {:?}...",
                            attempt, PEER_DISCOVERY_MAX_RETRIES, err, PEER_DISCOVERY_RETRY_DELAY
                        );

                        sleep(PEER_DISCOVERY_RETRY_DELAY).await;
                        attempt += 1;
                    }
                }
            }
        };

        debug!(
            "Found peer {} with chain info: {:?}",
            remote_peer.peer_id(),
            remote_chain_info
        );
        // fetch genesis from peer, compare or save locally
        self.ensure_genesis(&mut remote_peer).await?;

        let external_height = remote_chain_info.height;

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

        // ask the peer if its chain already includes our tip
        // this is an easy case for synchronization, since it just requires download and apply all the remaining blocks
        info!("Checking if peer includes our local tip in its canonical chain.");

        let peer_includes_local_tip =
            match remote_peer.request_block_by_height(local_tip_height).await {
                Ok(block) => block.block_hash() == local_tip_hash,
                Err(_) => false,
            };

        debug!("peer_includes_local_tip={}", peer_includes_local_tip);

        let sync_start_height = if peer_includes_local_tip {
            info!(
                "Peer {} includes our tip on its canonical chain. Simple sync from {} to {}.",
                remote_peer.peer_id(),
                local_tip_height + 1,
                external_height
            );

            local_tip_height + 1
        } else {
            info!(
                "Peer {} does not include our tip on its canonical chain. Finding LCA.",
                remote_peer.peer_id()
            );

            let (lca_height, lca_hash) = find_lca(
                &self.storage,
                &mut remote_peer,
                external_height,
                local_tip_height,
            )
            .await?;

            info!(
                "Successfully found Last-Common-Ancestor (LCA) block: height={}, hash={}. Syncing from {} to {}.",
                lca_height,
                lca_hash,
                lca_height + 1,
                external_height
            );

            lca_height + 1
        };

        let local_work = engine.tip().map(|(_, _, w)| w).unwrap_or(0);
        let remote_work = remote_chain_info.total_difficulty as u128;

        info!(
            "Local work={} vs remote work={} (heights: {} vs {})",
            local_work, remote_work, local_tip_height, external_height
        );

        info!(
            "Syncing with peer {} from height={} to height={}",
            remote_peer.peer_id(),
            sync_start_height,
            external_height,
        );

        run_ibd(
            &mut engine,
            &self.storage,
            &mut remote_peer,
            sync_start_height,
            external_height,
            self.sync_service_config.max_blocks_range_per_request,
        )
        .await?;

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
            if local_genesis_block != external_genesis_block {
                return Err(StryiNodeError::other(
                    "genesis mismatch: local and remote peer have different genesis blocks",
                ));
            }
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
            }

            info!("Genesis saved in meta and committed to storage (height=0).");
        }

        Ok(())
    }

    /// Finds the best peer to sync from.
    /// The best peer must pass invariant checks and have more work than us.
    /// Returns the peer with the highest total_difficulty, or an error if none qualify.
    async fn find_best_peer(&self) -> Result<(RemotePeer, ChainInfo), StryiNodeError> {
        let peers_list = query_sync_peers(&self.net_cmd, &self.sync_service_config)
            .await
            .map_err(|e| StryiNodeError::other(format!("failed to discover peers: {e}")))?;

        if peers_list.is_empty() {
            return Err(StryiNodeError::other(
                "no peers with compatible gRPC sync service found",
            ));
        }

        info!("Found {} peers with gRPC sync service.", peers_list.len());

        let local_work = self
            .storage
            .read()
            .await
            .chain_difficulty()
            .await
            .unwrap_or(0);

        info!("Local work: {}", local_work);

        let mut candidates: Vec<(RemotePeer, ChainInfo)> = Vec::new();

        for (peer_id, svc) in peers_list {
            info!("Checking peer {}...", peer_id);

            let channel = match connect_grpc(&svc).await {
                Ok(ch) => ch,
                Err(e) => {
                    warn!("Cannot connect to peer {}: {}. Skipping.", peer_id, e);
                    continue;
                }
            };

            let mut remote_peer = RemotePeer::new(peer_id, channel);

            let chain_info = match remote_peer.request_chain_info().await {
                Ok(ci) => ci,
                Err(e) => {
                    warn!(
                        "Cannot get chain info from peer {}: {}. Skipping.",
                        peer_id, e
                    );
                    continue;
                }
            };

            // Check protocol/chain name invariants
            if let Err(e) = self.validate_peer_chain_invariants(&chain_info).await {
                warn!("Peer {} failed invariant check: {}. Skipping.", peer_id, e);
                continue;
            }

            // the MAIN check: do we even need to sync with this peer?
            let remote_work = chain_info.total_difficulty as u128;
            if remote_work <= local_work {
                info!(
                    "Peer {} has no more work than us ({} <= {}) Skipping",
                    peer_id, remote_work, local_work
                );
                continue;
            }
            info!(
                "Peer {} is a valid candidate (its work={} which IS more than local_work={}, height={}).",
                peer_id, remote_work, local_work, chain_info.height
            );

            candidates.push((remote_peer, chain_info));
        }

        if candidates.is_empty() {
            return Err(StryiNodeError::other(
                "all peers have less or equal work than local chain - it's nothing to sync",
            ));
        }

        // Pick the peer with the highest total_difficulty
        let best = candidates
            .into_iter()
            .max_by_key(|(_, ci)| ci.total_difficulty as u128)
            .unwrap();

        info!(
            "Best peer selected: height={}, work={}",
            best.1.height, best.1.total_difficulty
        );

        Ok(best)
    }

    async fn validate_peer_chain_invariants(&self, info: &ChainInfo) -> Result<(), StryiNodeError> {
        require_chain_info_eq(
            "protocol_version",
            self.sync_service_config.protocol_version as u64,
            info.protocol_version as u64,
        )?;
        require_chain_info_eq(
            "chain_name",
            self.sync_service_config.chain_name.as_str(),
            info.chain_name.as_str(),
        )?;

        let remote_tip_hash = BlockHash::from_hash_string(&info.latest_block_hash)
            .map_err(|e| StryiNodeError::other(format!("invalid remote tip hash: {e}")))?;

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

        Ok(())
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
    let channel = grpc_endpoint_from_service(svc)?
        .connect()
        .await
        .map_err(|e| StryiNodeError::other(format!("gRPC dial error: {e:?}")))?;

    Ok(BlockchainSyncClient::new(channel))
}

pub(super) async fn query_sync_peers(
    net_cmd: &mpsc::Sender<NetworkCommand>,
    sync_config: &StryiSyncServiceConfig,
) -> Result<Vec<(PeerId, ServiceRecord)>, StryiNodeError> {
    let candidates = query_sync_candidates(net_cmd).await?;

    // filter only those which have a compatible sync api version.
    let res = candidates
        .iter()
        .filter(|(_, s)| s.version() as usize == sync_config.protocol_version)
        .map(|p| p.to_owned())
        .collect::<Vec<_>>();

    Ok(res)
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct GrpcConnectTarget {
    uri: tonic::transport::Uri,
    domain_name: String,
    cert_pem: String,
}

fn grpc_endpoint_from_service(svc: &ServiceRecord) -> Result<Endpoint, StryiNodeError> {
    let target = grpc_connect_target(svc)?;
    let tls_config = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(target.cert_pem.as_bytes()))
        .domain_name(target.domain_name);

    Endpoint::from(target.uri)
        .tls_config(tls_config)
        .map_err(StryiNodeError::from)
}

fn grpc_connect_target(svc: &ServiceRecord) -> Result<GrpcConnectTarget, StryiNodeError> {
    let domain_name = extract_tls_verification_host(svc.address())
        .map_err(|e| StryiNodeError::other(format!("gRPC TLS host error: {e}")))?;
    let uri = grpc_uri_from_multiaddr(svc.address(), "https")
        .map_err(|e| StryiNodeError::other(format!("gRPC URI error: {e}")))?;

    let cert_pem = match svc.transport_security() {
        ServiceTransportSecurity::TlsServerCert { cert_pem } => cert_pem.clone(),
        ServiceTransportSecurity::None => {
            return Err(StryiNodeError::other(
                "gRPC sync service must advertise TLS metadata",
            ));
        }
    };

    Ok(GrpcConnectTarget {
        uri,
        domain_name,
        cert_pem,
    })
}

/// Converts multiaddr to grpc uri
fn grpc_uri_from_multiaddr(
    addr: &Multiaddr,
    scheme: &str,
) -> Result<tonic::transport::Uri, String> {
    let host = extract_tls_verification_host(addr)?;
    let mut port: Option<u16> = None;

    for proto in addr.iter() {
        if let Protocol::Tcp(p) = proto {
            port = Some(p);
        }
    }

    let port = port.ok_or("multiaddr missing tcp port")?;

    let uri_str = format!("{scheme}://{host}:{port}");

    uri_str
        .parse::<tonic::transport::Uri>()
        .map_err(|e| format!("invalid grpc uri '{}': {e}", uri_str))
}

async fn run_ibd(
    engine: &mut StryiConsensusEngine<StryiStorage>,
    storage: &Arc<RwLock<StryiStorage>>,
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

        ingest_ibd_batch(engine, storage, downloaded_blocks).await?;

        current_height += fetched_count;
    }

    Ok(())
}

/// Builds ConsensusConstants instance, calculates current difficulty from tip, other stuff from config.
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
