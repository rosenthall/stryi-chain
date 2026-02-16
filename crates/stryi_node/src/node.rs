use crate::bootstrap::GenesisBootstrap;
use crate::error::StryiNodeError;
use crate::grpc::{GRPC_SERVICE_TAG, StryiSyncService, StryiSyncServiceConfig};
use crate::grpc_services::blockchain_sync_client::BlockchainSyncClient;
use crate::grpc_services::blockchain_sync_server::BlockchainSyncServer;
use crate::grpc_services::{BlockHashList, ChainInfo};
use crate::http::{HTTP_SERVICE_TAG, StryiHttpServiceConfig};
use crate::ibd::{fetch_blocks_batch, ingest_ibd_batch};
use crate::middleware::ready::{ReadyFlag, ReadyGateLayer};
use crate::tls::NodeTlsIdentity;
use multiaddr::{Multiaddr, Protocol};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use stryi_core::address::AccountAddress;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::{
    BlockValidator, ConsensusConsts, ConsensusEngine, ConsensusVerdict, StryiConsensusEngine,
};
use stryi_core::difficulty::build_difficulty_calculator_from_consts;
use stryi_core::mempool::MemPool;
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use stryi_core::transactions::{OutPoint, UTXO, UtxoProcessor};
use stryi_network::ed25519::Keypair;
use stryi_network::{
    BroadcastBlock, NetworkCommand, NetworkEvent, PeerId, ServiceRecord, StryiNetworkError,
    StryiNetworkManager,
};
use stryi_storage::{StryiStorage, extract_utxos_from_block};
use tokio::join;
use tokio::sync::broadcast::Sender;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio::time::{Instant, sleep};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Server, ServerTlsConfig};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, trace, warn};

/// All the channels and metadata that connect the miner to the node event loop.
/// Only present when mining is enabled for this node.
pub(crate) struct MinerBridge {
    /// Receiver for blocks mined locally.
    pub mined_blocks_receiver: mpsc::Receiver<Block>,
    /// Miner's reward address, needed to wrap mined blocks for gossipsub.
    pub miner_address: AccountAddress,
}

/// The main struct representing the Stryi node instance.
/// This node will later integrate networking, consensus, gRPC sync, mempool and mining services.
pub struct StryiChainNode {
    // TODO: Integrate consensus engine, mining loop manager, ...
    /// The blockchain storage (UTXO set, block storage, undo data, tc.)
    pub(crate) storage: Arc<RwLock<StryiStorage>>,

    /// Consensus Engine instance.
    /// The `synchronize()` stage setups consensus engine.
    /// We need this, because before synchronization/connecting to the network we don't know some values we need
    /// to build ConsensusConstants instance.
    /// They depended on genesis(which may be external), current chain state (that we don't have until sync is complete), etc.
    pub(crate) consensus_engine: Option<Arc<Mutex<StryiConsensusEngine<StryiStorage>>>>,

    /// Channel for sending block tip updates immediately after a new block is added to the chain.
    /// Miner (and some other services in the future) rely on these updates.
    pub(crate) tip_updates_sender: Sender<BlockHash>,

    /// Miner-to-node bridge: channels and metadata. None when mining is disabled.
    pub(crate) miner_bridge: Option<MinerBridge>,

    /// Mempool instance.
    pub(crate) mempool: Arc<RwLock<MemPool>>,

    /// The blockchain's p2p layer, instance of StryiNetworkManager that allows to communicate with other nodes
    // Network manager is owned until connect(); then moved into the run loop task.
    pub(crate) network_manager: Option<StryiNetworkManager>,

    /// The node's identity keypair (ed25519) from NetworkManager
    pub(crate) keypair: Keypair,

    /// The peer id of this node.
    pub(crate) peer_id: PeerId,

    /// A node's tls identity (based on PeerKey)
    pub(crate) tls_identity: NodeTlsIdentity,

    /// The root certificate for TLS connections to other peers (for gRPC client)
    pub(crate) grpc_tls_root: tonic::transport::Certificate,

    // Lightweight handles that live after connect().
    /// The command channel for sending network commands to the network manager.
    pub(crate) net_cmd: Option<mpsc::Sender<NetworkCommand>>,

    /// The network events channel, used to receive events from the network manager.
    pub(crate) net_events: Option<broadcast::Receiver<NetworkEvent>>,

    /// Genesis bootstrap orchestrator
    pub(crate) genesis_bootstrap: GenesisBootstrap,

    // Configuration for the gRPC-based synchronization service.
    pub(crate) sync_service_config: StryiSyncServiceConfig,

    /// Configuration for a high-level http api service for node's users.
    pub(crate) http_service_config: StryiHttpServiceConfig,

    /// The readiness flag used by the gRPC middleware.
    pub(crate) grpc_is_ready: ReadyFlag,

    /// The readiness flag used by the http middleware.
    pub(crate) http_is_ready: ReadyFlag,
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

impl StryiChainNode {
    /// Setter method for ConsensusEngine
    pub(crate) fn set_consensus_engine(&mut self, engine: StryiConsensusEngine<StryiStorage>) {
        self.consensus_engine = Some(Arc::new(Mutex::new(engine)));
    }

    /// connect() is the first step in the node's lifecycle.
    /// It initializes the network manager and connects to the network.
    /// It must be called before any other operations, like synchronization or starting services.
    pub(crate) async fn connect(&mut self) -> Result<(), StryiNodeError> {
        info!("Connecting to the network...");

        let mut mgr = self
            .network_manager
            .take()
            .ok_or_else(|| StryiNodeError::other("network manager not initialized"))?;

        let cmd = mgr.command_sender();
        let events = mgr.subscribe_events();

        // run the network manager in a separate task
        tokio::spawn(async move { mgr.run_loop().await });

        // Store the command sender and events receiver in the node
        self.net_cmd = Some(cmd);
        self.net_events = Some(events);

        Ok(())
    }

    // Constants for peer discovery during synchronization

    /// How many seconds try to discover peers with gRPC sync service
    const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

    /// How often to poll the network for peers with gRPC sync service
    const DISCOVERY_INTERVAL: Duration = Duration::from_millis(250);

    /// synchronize() is the second step in the node's lifecycle.
    /// It is responsible for synchronizing the node with the network, fetching blocks, transactions,
    /// and other data needed to bring the node up to date.
    /// Also, it builds self.consensus_engine and sets the field.
    pub(crate) async fn synchronize(&mut self) -> Result<(), StryiNodeError> {
        info!("Start synchronizing with the network...");

        let net_cmd = self
            .net_cmd
            .as_ref()
            .ok_or_else(|| StryiNodeError::other("network not connected"))?;

        let started = Instant::now();
        let mut candidates: Vec<(PeerId, ServiceRecord)> = Vec::new();

        // First, discover peers that offer the gRPC sync service with the compatible version
        // not fail instantly if none found - retry for up to DISCOVERY_TIMEOUT
        loop {
            let (tx, rx) = tokio::sync::oneshot::channel();
            net_cmd
                .send(NetworkCommand::QueryPeersWithService {
                    service: GRPC_SERVICE_TAG.to_owned(),
                    respond_to: tx,
                })
                .await
                .map_err(|_| StryiNodeError::other("network command channel closed"))?;

            if let Ok(mut v) = rx.await {
                v.retain(|(_, s)| {
                    s.version() as usize == self.sync_service_config.protocol_version
                });
                if !v.is_empty() {
                    candidates = v;
                    break;
                }
            }

            if started.elapsed() >= Self::DISCOVERY_TIMEOUT {
                break;
            }
            sleep(Self::DISCOVERY_INTERVAL).await;
        }

        if candidates.is_empty() {
            return Err(StryiNodeError::other(
                "No compatible gRPC sync service found",
            ));
        }

        debug!("Candidates: {:#?}", candidates);

        let (peer, svc) = candidates[0].clone();

        /*
        // TODO: Turn back service's validation in synchronization process
        // obtain peer's libp2p public key
        let peer_pubkey = {
            let (tx, rx) = tokio::sync::oneshot::channel();
            net_cmd
                .send(NetworkCommand::QueryPeerPublicKey { peer, respond_to: tx })
                .await
                .map_err(|_| StryiNodeError::other("network command channel closed"))?;

            rx.await
                .map_err(|_| StryiNodeError::other("network response channel closed"))?
                .ok_or_else(|| StryiNodeError::other("peer not found"))?
        };


        // verify the signature on the service info using the peer's public key
        // convert to ed25519 public key
        let peer_pubkey = peer_pubkey.try_into_ed25519().expect("peer public key is not ed25519");
        if !svc.verify_signature(&peer_pubkey) {
            return Err(StryiNodeError::other("TLS certificate signature invalid"));
        }
        */

        info!(
            "Using gRPC sync service at {} (version {})",
            svc.address(),
            svc.version()
        );

        let (grpc_peer_id, grpc_peer_service_info) = candidates
            .into_iter()
            .find(|(_peer, svc)| {
                trace!("peer's service info: {:?}", svc);
                trace!(
                    "Discovered service version: {}, required: {}",
                    svc.version(),
                    self.sync_service_config.protocol_version
                );
                svc.version() as usize == self.sync_service_config.protocol_version
            })
            .ok_or_else(|| StryiNodeError::other("No compatible gRPC sync service found"))?;

        info!(
            "Using gRPC sync service from peer {} at address {} with version {}",
            grpc_peer_id,
            grpc_peer_service_info.address(),
            grpc_peer_service_info.version()
        );

        // Connect to gRPC

        // Parse the address into a tonic::transport::Uri
        let uri = grpc_uri_from_multiaddr(grpc_peer_service_info.address())
            .map_err(StryiNodeError::other)?;

        let channel = tonic::transport::Endpoint::from(uri)
            .connect()
            .await
            .map_err(|e| StryiNodeError::other(format!("gRPC dial error: {e}")))?;

        let mut grpc_client = BlockchainSyncClient::new(channel);

        // get external peer's chain info
        info!("Requesting Chain Info from external peer so we can compare it with local one.");
        let external_chain_info = grpc_client
            .get_chain_info(tonic::Request::new(()))
            .await
            .map_err(|e| {
                StryiNodeError::other(format!("Failed to get chain info from gRPC service: {e}"))
            })?;
        let external_chain_info = external_chain_info.into_inner();
        debug!(external_chain_info = ?external_chain_info);

        // validate it
        self.validate_peer_chain_info(external_chain_info.clone())
            .await?;

        // request the genesis block from the gRPC sync service
        // if we already have genesis in local storage - we will compare it to external one
        // if we have no genesis locally - prompt user and ask if we can accept and save this block
        info!(
            "Requesting genesis block from gRPC sync service at {}",
            svc.address()
        );

        let external_genesis_block: Block = {
            let response = grpc_client
                .get_blocks_by_hash(BlockHashList {
                    block_hashes: vec![BlockHash::empty().to_string()],
                })
                .await
                .map_err(|e| {
                    StryiNodeError::other(format!(
                        "Failed to get genesis block from gRPC service: {e}"
                    ))
                })?;

            // `get_blocks_by_hash` returns a stream; pull first element
            let genesis_block_grpc = response
                .into_inner()
                .next()
                .await
                .ok_or_else(|| StryiNodeError::other("No genesis block returned"))?
                .map_err(|e| {
                    StryiNodeError::other(format!(
                        "Received error status while fetching genesis: {e}"
                    ))
                })?;

            genesis_block_grpc.try_into().map_err(|e| {
                StryiNodeError::other(format!(
                    "Failed to convert gRPC block to internal type: {e:?}"
                ))
            })?
        };

        trace!("Received genesis block: {:?}", external_genesis_block);

        // If storage has no genesis yet, save meta first, then commit the exact same block.
        // This keeps a single source of truth and crash-safety (meta before state).
        let maybe_local_genesis = {
            let s = self.storage.read().await;
            // Expect a storage API able to check height 0 presence; adjust if your API differs.
            s.get_block_by_height(0).await.map_err(|e| {
                StryiNodeError::other(format!("failed to query storage for genesis: {e}"))
            })?
        };

        let need_genesis = maybe_local_genesis.is_none();

        if need_genesis {
            // Save meta
            self.genesis_bootstrap
                .confirm_and_save(
                    &external_genesis_block,
                    &self.sync_service_config.chain_name,
                    self.sync_service_config.protocol_version as u64,
                    Some(&*format!("peer {peer}")),
                    true,
                )
                .map_err(|e| StryiNodeError::other(format!("confirm_and_save failed: {e}")))?;

            // Commit the exact same block to storage.
            {
                let mut s = self.storage.write().await;
                s.put_block(&external_genesis_block).await.map_err(|e| {
                    StryiNodeError::other(format!("failed to commit genesis block: {e}"))
                })?;

                info!("Genesis block successfully stored.");
                // And utxos via put_utxos
                let utxos_to_insert: Vec<(OutPoint, UTXO)> =
                    extract_utxos_from_block(&external_genesis_block);

                s.batch_put_utxos(utxos_to_insert).await.map_err(|e| {
                    StryiNodeError::other(format!("failed to commit genesis UTXOs: {e}"))
                })?;

                info!("Genesis UTXOs successfully stored.");

                info!("OMG IT WORKED")
            }

            info!("Genesis saved in meta and committed to storage (height=0).");
        } else {
            // if genesis is already set - compare it to one we got from peer
            let local_genesis_block = maybe_local_genesis.expect("already checked");
            let external_genesis_block = external_genesis_block.clone();

            trace!(local_genesis = ?local_genesis_block, external_genesis = ?external_genesis_block);
            assert_eq!(
                local_genesis_block, external_genesis_block,
                "different genesis blocks detected locally and in this peer! currently unsupported"
            )
        }

        /*
          ___ _   _ _ __   ___
         / __| | | | '_ \ / __|
         \__ \ |_| | | | | (__
         |___/\__, |_| |_|\___|
               __/ |
              |___/
        github.com/rosenthall :>
        */

        // Build ConsensusEngine instance

        info!("Trying to instantize StryiConsensusEngine instance");

        let consensus_constants = build_consensus_constants(&self.storage.clone()).await?;
        let difficulty_calculator =
            build_difficulty_calculator_from_consts::<StryiStorage>(consensus_constants);

        let block_validator =
            BlockValidator::new(consensus_constants, difficulty_calculator.clone());
        let utxo_processor = UtxoProcessor::new();
        trace!(consensus_constants = ?consensus_constants);

        // NOTE: after synchronizing complete, we shall set self.consensus_engine value.
        let mut engine = StryiConsensusEngine::new(
            consensus_constants,
            block_validator,
            utxo_processor,
            self.storage.clone(),
            difficulty_calculator,
        )
        .await
        .map_err(|e| StryiNodeError::other(format!("consensus engine init failed: {e}")))?;

        info!("Success!");

        // Now querying all the blocks we need from peer to have the same chain.
        let (local_tip_height, local_tip_hash) = {
            let s = self.storage.read().await;
            let tip = s
                .tip()
                .await
                .map_err(|e| StryiNodeError::other(format!("tip(): {e}")))?;
            (tip.0, tip.1)
        };

        trace!(local_tip_height = ?local_tip_height, local_tip_hash = ?local_tip_hash);

        // Firstly, ask the peer if its chain already includes our TIP
        info!("Checking if peer has our local tip included in its chain.");

        // note : I'm not sure how it will behave when only common block is genesis.
        let peer_includes_local_tip =
            StryiChainNode::has_remote_block_by_hash(&mut grpc_client, local_tip_hash).await?;
        if peer_includes_local_tip {
            info!(
                "Success! peer {} knows block {} (which is our local tip)! Downloading the rest of the blocks..",
                peer, local_tip_hash
            );

            let external_height = external_chain_info.height;

            let heights_differ = external_height
                .checked_sub(local_tip_height)
                .expect("Local height cannot be higher than external one at this point.")
                as usize;

            // calculate the maximal batch size for requesting blocks we need.
            // If we only need less blocks than `max_blocks_range_per_request` from config - set and download it all like that.
            let batch_size = std::cmp::min(
                self.sync_service_config.max_blocks_range_per_request,
                heights_differ,
            );

            let (start_height, end_height) = (local_tip_height + 1, external_height);

            let downloaded_blocks =
                fetch_blocks_batch(&mut grpc_client, batch_size, start_height, end_height).await?;

            ingest_ibd_batch(&mut engine, downloaded_blocks)
                .await
                .map_err(|e| {
                    StryiNodeError::other(format!("Got critical error during IBD process : {e}"))
                })?;

            // put ConsensusEngine in place
            self.set_consensus_engine(engine);

            info!("IBD complete, node is now synchronized! Ready to start own services.");

            return Ok(());
        }

        // If our tip is not included in other peer's chain - it is way harder to find LCA.
        // We use some binary-search-ish algorithm for that purpose to reduce RPC calls amount and
        // find LCA in just O(log n) requests, which is about 20 steps for searching in 1_000_000 blocks.

        info!(
            "Peer {} does not have our local tip {}. Finding LCA via binary search.",
            peer, local_tip_hash
        );

        let external_height = external_chain_info.height;

        let (lca_height, lca_hash) = self
            .find_last_common_ancestor(&mut grpc_client, external_height, local_tip_height)
            .await?;

        info!(
            "LCA found: height={}, hash={}. Chains start differ since height {}.",
            lca_height,
            lca_hash,
            lca_height + 1
        );

        if external_height == lca_height {
            info!(
                "Remote tip is at LCA height {}. Local chain (height {}) is ahead. Nothing to sync.",
                lca_height, local_tip_height
            );
            self.set_consensus_engine(engine);
            // TODO: Make other nodes try to synchronize with local one when local has better height immediately

            return Ok(());
        }

        let start_height = lca_height + 1;
        let end_height = external_height;
        let blocks_to_download = (end_height - start_height + 1) as usize;

        let batch_size = std::cmp::min(
            self.sync_service_config.max_blocks_range_per_request,
            blocks_to_download,
        );

        let downloaded_blocks =
            fetch_blocks_batch(&mut grpc_client, batch_size, start_height, end_height).await?;

        info!(
            "Downloaded {} blocks [{}, {}]. Feeding to consensus engine for fork resolution.",
            downloaded_blocks.len(),
            start_height,
            end_height
        );

        ingest_ibd_batch(&mut engine, downloaded_blocks)
            .await
            .map_err(|e| {
                StryiNodeError::other(format!("Critical error during fork resolution: {e}"))
            })?;

        self.set_consensus_engine(engine);

        
        info!("Finished synchronization with the network!");
        

        Ok(())
    }

    // simple helper to validate local values against ones from peer
    #[inline]
    fn require_chain_info_eq<T>(
        field: &'static str,
        local: T,
        remote: T,
    ) -> Result<(), StryiNodeError>
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

    /// Validate remote ChainInfo against local configuration and current local tip.
    /// Returns (remote_height, remote_tip_hash) if validation passes.
    pub(crate) async fn validate_peer_chain_info(
        &self,
        info: ChainInfo,
    ) -> Result<(u64, BlockHash), StryiNodeError> {
        // Hard invariants: protocol version and chain name must match.

        let local_proto: u64 = self.sync_service_config.protocol_version as u64;
        let remote_proto: u64 = info.protocol_version as u64;
        Self::require_chain_info_eq("protocol_version", local_proto, remote_proto)?;

        let local_chain: &str = self.sync_service_config.chain_name.as_str();
        let remote_chain: &str = info.chain_name.as_str();
        Self::require_chain_info_eq("chain_name", local_chain, remote_chain)?;

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
                "tip_hash@same_height",
                local_tip_hash.to_string(),
                remote_tip_hash.to_string(),
            ));
        }

        // warn if peer is behind
        if info.height < local_height {
            warn!(
                "peer behind: remote_height={}, local_height={}",
                info.height, local_height
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

    /// Starts the node instance and basic services, like mempool, grpc sync server, mining-loop (if set in the config), handles network events
    /// Meant to be called after `connect()` and `synchronize()`.
    /// Takes `http_advertise_address` and `grpc_advertise_address` as parameters;
    /// these are the addresses that other nodes will use to connect to this node's services.
    /// They will be registered and signed by the network manager.
    pub async fn start_services(
        self,
        http_advertise_address: Multiaddr,
        grpc_advertise_address: Multiaddr,
        cancel_token: CancellationToken,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Some assertions, just in case.
        assert!(
            &self.consensus_engine.is_some(),
            "ConsensusEngine must be initialized before start_services()"
        );
        assert!(
            &self.network_manager.is_none(),
            "NetworkManager's loop must be spawned in connect(), so it must be unaccessible in start_services()"
        );
        assert!(
            self.net_cmd.is_some() && self.net_events.is_some(),
            "net_cmd and net_events fields shall be initialized before start_services()"
        );

        // Destructure to avoid partial borrows
        // After this - there will be no more "self" itself, but just all the fields/values separated
        let StryiChainNode {
            mempool,
            storage,
            tip_updates_sender,
            miner_bridge,
            net_cmd,
            net_events,
            consensus_engine,
            peer_id,
            tls_identity,

            sync_service_config,
            http_service_config,
            grpc_is_ready,
            http_is_ready,
            ..
        } = self;

        // Unpack the optional miner bridge into a separate receiver and address
        let (mut mined_blocks_receiver, miner_address) = match miner_bridge {
            Some(mb) => (Some(mb.mined_blocks_receiver), Some(mb.miner_address)),
            None => (None, None),
        };

        // safely unwrap fields that must be initialized before start_services()
        let net_cmd = net_cmd.expect("net_cmd must be initialized before start_services()");
        let mut net_events =
            net_events.expect("net_events must be initialized before start_services()");
        let consensus_engine =
            consensus_engine.expect("consensus_engine must be initialized before start_services()");

        // clone once per task
        let storage_for_http = Arc::clone(&storage);
        let storage_for_grpc = Arc::clone(&storage);

        // http server future
        let http_cancel = cancel_token.child_token();
        let http_fut = async {
            crate::http::start_http_server(
                storage_for_http,
                http_service_config.clone(),
                mempool.clone(),
                http_is_ready,
                http_cancel,
            )
            .await
        };

        // gRPC server future
        // TODO: Make gRPC really use tls based on provider peer's identity keys
        let grpc_cancel = cancel_token.child_token();
        let grpc_fut = async {
            // Create the sync service instance
            let service_impl = StryiSyncService {
                config: sync_service_config.clone(),
                storage: storage_for_grpc,
            };

            let tonic_identity =
                tonic::transport::Identity::from_pem(&tls_identity.cert_pem, &tls_identity.key_pem);
            let _tls_config = ServerTlsConfig::new().identity(tonic_identity);

            // Wrap the instance in Tonic’s generated server
            let svc = BlockchainSyncServer::new(service_impl);

            // Build the readiness layer middleware.
            let readiness_layer = ReadyGateLayer::new(grpc_is_ready.clone());

            // wrap it up
            let svc = ServiceBuilder::new().layer(readiness_layer).service(svc);

            info!(
                "Starting gRPC sync service on {}",
                &sync_service_config.address
            );

            // Start serving the sync service on the configured port,
            // with graceful shutdown
            Server::builder()
                // .tls_config(tls_config).unwrap()
                // Compress responses
                .layer(CompressionLayer::new())
                // High-level logging of requests and responses
                .layer(TraceLayer::new_for_grpc())
                .add_service(svc)
                .serve_with_shutdown(sync_service_config.address, grpc_cancel.cancelled())
                .await
        };

        // Register gRPC and HTTP service in ServiceRecords
        {
            let grpc_record = ServiceRecord::new(
                grpc_advertise_address,
                peer_id,
                GRPC_SERVICE_TAG.to_string(),
                sync_service_config.protocol_version as u32,
            );

            let http_record = ServiceRecord::new(
                http_advertise_address,
                peer_id,
                HTTP_SERVICE_TAG.to_string(),
                http_service_config.api_version,
            );

            info!("Successfully signed node's http service with own keypair!");

            // Register both services by sending messages to network_manager

            // helper closure
            let register = async |service: ServiceRecord| -> Result<(), StryiNetworkError> {
                let (respond_to, receive_here) =
                    tokio::sync::oneshot::channel::<Result<(), StryiNetworkError>>();

                net_cmd
                    .send(NetworkCommand::AddService {
                        service: service.clone(),
                        respond_to,
                    })
                    .await
                    .map_err(|e| {
                        StryiNetworkError::other(format!(
                            "Failed to send AddService message: {:?}",
                            e
                        ))
                    })?;

                let _ = receive_here.await.map_err(|e| {
                    StryiNetworkError::other(format!(
                        "Failed to receive AddService response: {:?}",
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

        // Network event consumer loop - routes
        // 1. incoming blocks to consensus engine
        // 2. incoming transactions to mempool
        // And logs other events
        let event_cancel = cancel_token.child_token();
        let mempool_for_events = Arc::clone(&mempool);

        // Helper: recv from an optional channel, or pend forever if mining is disabled in config.
        async fn recv_mined_block(rx: &mut Option<mpsc::Receiver<Block>>) -> Option<Block> {
            match rx {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        }

        let event_loop_fut = async move {
            loop {
                tokio::select! {

                    // -- local miner produced a block --
                    Some(mined_block) = recv_mined_block(&mut mined_blocks_receiver) => {
                        info!("=========================================");
                        info!("LOCAL MINER HAS MINED A BLOCK: ");
                        info!("Hash: {}", mined_block.block_hash());
                        info!("Merkle Root: {}", mined_block.header.merkle_root_hash);
                        info!("Transactions: {}", mined_block.data.transactions.len());
                        info!("=========================================");

                        // Validate through consensus engine, same as network blocks
                        let mut engine = consensus_engine.lock().await;
                        match engine.on_block(mined_block.clone()).await {
                            Ok(verdict) => {
                                info!("Mined block consensus verdict: {:?}", verdict);

                                if matches!(verdict,
                                    ConsensusVerdict::Applied { .. } | ConsensusVerdict::CausedReorganization { .. }
                                ) {
                                    let _ = tip_updates_sender.send(mined_block.block_hash());
                                    drop(engine);

                                    // Clean confirmed transactions from the mempool
                                    let mut pool = mempool_for_events.write().await;
                                    if let Err(e) = pool.update_on_block(mined_block.data.clone()).await {
                                        warn!("Failed to clean mempool after mined block: {:?}", e);
                                    }
                                    drop(pool);

                                    // Broadcast to the network
                                    if let Some(addr) = miner_address {
                                        let first_seen = SystemTime::now()
                                            .duration_since(SystemTime::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_secs();
                                        let wrapped = BroadcastBlock::new(mined_block, addr, first_seen);
                                        let _ = net_cmd.send(NetworkCommand::PublishBlock(wrapped)).await;
                                    }
                                } else {
                                    warn!("Mined block not applied (verdict: {:?}), discarding.", verdict);
                                }
                            }
                            Err(e) => warn!("Mined block rejected by consensus: {:?}", e),
                        }
                    }

                    // events from the network
                    result = net_events.recv() => {
                        match result {
                            Ok(NetworkEvent::NewBlock(broadcast_block)) => {
                                info!(
                                    "Received block #{} ({}) from network",
                                    broadcast_block.block.header.height,
                                    broadcast_block.block.block_hash()
                                );
                                let block = broadcast_block.block;
                                let mut engine = consensus_engine.lock().await;
                                match engine.on_block(block.clone()).await {
                                    Ok(verdict) => {
                                        info!("Consensus verdict: {:?}", verdict);

                                        // check if the block was applied
                                        if matches!(verdict,
                                            ConsensusVerdict::Applied { .. } | ConsensusVerdict::CausedReorganization { .. }
                                        ) {

                                            debug!("Block applied, updating tip and clearing mempool.");
                                            let _ = tip_updates_sender.send(block.block_hash());

                                            // Clean confirmed transactions from the mempool's internal state
                                            // Drop engine lock before acquiring mempool lock
                                            drop(engine);
                                            let mut pool = mempool_for_events.write().await;
                                            if let Err(e) = pool.update_on_block(block.data).await {
                                                warn!("Failed to clean mempool after block: {:?}", e);
                                            }
                                        }
                                    }
                                    Err(e) => warn!("Block rejected by consensus: {:?}", e),
                                }
                            }
                            Ok(NetworkEvent::NewTransaction(tx)) => {
                                debug!("Received transaction from network: {:?}", tx.data.hash());
                                let mut pool = mempool_for_events.write().await;
                                match pool.add_transaction(tx).await {
                                    Ok(()) => debug!("Transaction added to mempool"),
                                    Err(e) => debug!("Transaction rejected by mempool: {:?}", e),
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

        // run all the services concurrently
        let (grpc_res, _http_res, _event_res) = join!(grpc_fut, http_fut, event_loop_fut);
        grpc_res?; // propagate gRPC error if any

        Ok(())
    }
}

// Builds ConsensusConstants instance, calculates current difficulty from tip, other stuff from config.
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
