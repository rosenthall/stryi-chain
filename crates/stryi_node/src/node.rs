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
use std::sync::Arc;
use std::time::Duration;
use stryi_core::block::{Block, BlockHash};
use stryi_core::consensus::{BlockValidator, ConsensusConsts, StryiConsensusEngine};
use stryi_core::difficulty::{DifficultyCalc, build_difficulty_calculator_from_consts};
use stryi_core::mempool::MemPool;
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use stryi_core::transactions::{OutPoint, UTXO, UtxoProcessor};
use stryi_network::ed25519::Keypair;
use stryi_network::{
    NetworkCommand, NetworkEvent, PeerId, ServiceRecord, SignedServiceRecord, StryiNetworkManager,
};
use stryi_storage::{StryiStorage, extract_utxos_from_block};
use tokio::join;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio::time::{Instant, sleep};
use tokio_stream::StreamExt;
use tonic::transport::{Endpoint, Server, ServerTlsConfig};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, trace, warn};

/// The main struct representing the Stryi node instance.
/// This node will later integrate networking, consensus, gRPC sync, mempool and mining services.
pub struct StryiChainNode {
    // TODO: Integrate consensus engine, mining loop manager, ...
    /// The blockchain storage (UTXO set, block storage, undo data etc.)
    pub(crate) storage: Arc<RwLock<StryiStorage>>,

    /// Consensus Engine instance.
    /// The `synchronize()` stage setups consensus engine.
    /// We need this, because before synchronization/connecting to the network we don't know some values we need
    /// to build ConsensusConstants instance.
    /// They're depended on genesis(which may be external), current chain state (that we don't have until sync is complete), etc.
    pub(crate) consensus_engine: Option<Mutex<StryiConsensusEngine<StryiStorage>>>,

    /// Mempool object.
    pub(crate) mempool: Arc<RwLock<MemPool>>,

    /// The blockchain's p2p layer, instance of StryiNetworkManager that allows to communicate with other nodes
    // Network manager is owned until connect(); then moved into the run loop task.
    pub(crate) network_manager: Option<StryiNetworkManager>,

    /// The node's identity keypair (ed25519) from NetworkManager
    pub(crate) keypair: Keypair,

    /// The peer id of this node.
    pub(crate) peer_id: PeerId,

    /// A list of services that this node runs
    pub(crate) services_records: Arc<RwLock<Vec<SignedServiceRecord>>>,

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

    /// Configuration for the gRPC-based synchronization service.
    pub(crate) sync_service_config: StryiSyncServiceConfig,

    /// Configuration for high-level http api service for node's users.
    pub(crate) http_service_config: StryiHttpServiceConfig,

    /// The readiness flag used by the gRPC middleware.
    pub(crate) grpc_is_ready: ReadyFlag,

    /// The readiness flag used by the http middleware.
    pub(crate) http_is_ready: ReadyFlag,
}

impl StryiChainNode {
    /// Setter method for ConsensusEngine
    pub(crate) fn set_consensus_engine(&mut self, engine: StryiConsensusEngine<StryiStorage>) {
        let tmp = Mutex::new(engine);
        self.consensus_engine = Some(tmp);
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
    /// Also it builds self.consensus_engine and sets the field.
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

        // Connect to gRPC !

        // Parse the address into a tonic::transport::Uri
        let formatted_addr = format!("http://{}", grpc_peer_service_info.address());

        let uri = formatted_addr
            .parse::<tonic::transport::Uri>()
            .map_err(|e| StryiNodeError::other(format!("Failed to parse URI: {e}")))?;

        // create TLS channel and gRPC client
        let channel = Endpoint::from(uri)
            // .tls_config(tls_cfg)
            // .map_err(|e| StryiNodeError::other(format!("TLS config error: {e}")))?
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
            // Save meta (single place to "save" the network binding).
            self.genesis_bootstrap
                .confirm_and_save(
                    &external_genesis_block,
                    &self.sync_service_config.chain_name,
                    self.sync_service_config.protocol_version as u64,
                    Some(&*format!("peer {peer}")),
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

        // Firstly, ask peer if its chain already includes our TIP
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
        // find LCA in just O(log n) requests, which is about 20 steps for searching in 1_000_00 blocks.

        // TODO: Integrate LCA implementation in sync()

        todo!(
            "Cannot perform IBD/sync process if another node has no our tip already included in its chain yet "
        );
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
    pub async fn start_services(self) -> Result<(), Box<dyn std::error::Error>> {
        // Some asserts, just in case.
        assert!(
            &self.consensus_engine.is_some(),
            "ConsensusEngine must be initialized before start_services()"
        );
        assert!(
            &self.network_manager.is_none(),
            "NetworkManager's loop must be spawned in connect(), so it must be unaccessable in start_services()"
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
            network_manager: _network_manager,
            keypair,
            peer_id,
            services_records,
            tls_identity,

            sync_service_config,
            http_service_config,
            grpc_is_ready,
            http_is_ready,
            ..
        } = self;

        // clone once per task
        let storage_for_http = Arc::clone(&storage);
        let storage_for_grpc = Arc::clone(&storage);

        // http server future
        let http_fut = async {
            crate::http::start_http_server(
                storage_for_http,
                http_service_config.clone(),
                mempool.clone(),
                http_is_ready,
            )
            .await
        };

        // gRPC server future
        // TODO: Make gRPC really use tls based on provider peer's identity keys
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

            // Start serving the sync service on the configured port
            Server::builder()
                // .tls_config(tls_config).unwrap()
                // Compress responses
                .layer(CompressionLayer::new())
                // High level logging of requests and responses
                .layer(TraceLayer::new_for_grpc())
                .add_service(svc)
                .serve(sync_service_config.address)
                .await
        };

        // Register gRPC and HTTP service in ServiceRecords
        {
            let grpc_record = ServiceRecord::new(
                sync_service_config.address,
                peer_id,
                GRPC_SERVICE_TAG.to_string(),
                sync_service_config.protocol_version as u32,
            );

            let signed_grpc_record = SignedServiceRecord::sign(keypair.clone(), grpc_record)?;

            info!("Successfully signed node's gRPC service with own keypair!");

            let http_record = ServiceRecord::new(
                http_service_config.address,
                peer_id,
                HTTP_SERVICE_TAG.to_string(),
                http_service_config.api_version,
            );
            let signed_http_record = SignedServiceRecord::sign(keypair.clone(), http_record)?;

            info!("Successfully signed node's http service with own keypair!");

            services_records.write().await.push(signed_grpc_record);
            services_records.write().await.push(signed_http_record);
        }

        // run all the services concurrently
        let (grpc_res, _http_res) = join!(grpc_fut, http_fut);
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

/// Build a consensus engine using dynamic fields from the current tip.
/// Assumes genesis is already committed (height 0 present).
pub(crate) async fn build_consensus_engine(
    storage: Arc<RwLock<StryiStorage>>,
    difficulty_calc: DifficultyCalc<StryiStorage>,
) -> Result<StryiConsensusEngine<StryiStorage>, StryiNodeError> {
    let rules = build_consensus_constants(&storage.clone()).await?;
    let block_validator = BlockValidator::new(rules, difficulty_calc.clone());
    let utxo_processor = UtxoProcessor::new();

    let engine = StryiConsensusEngine::new(
        rules,
        block_validator,
        utxo_processor,
        storage.clone(),
        difficulty_calc,
    )
    .await
    .map_err(|e| StryiNodeError::other(format!("consensus engine init failed: {e}")))?;

    Ok(engine)
}
