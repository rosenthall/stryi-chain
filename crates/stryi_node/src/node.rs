use crate::error::StryiNodeError;
use crate::grpc::{StryiSyncService, StryiSyncServiceConfig, GRPC_SERVICE_TAG};
use crate::grpc_services::blockchain_sync_server::BlockchainSyncServer;
use crate::http::{StryiHttpServiceConfig, HTTP_SERVICE_TAG};
use crate::middleware::{ReadyFlag, ReadyGateLayer};
use crate::tls::NodeTlsIdentity;
use std::sync::Arc;
use std::time::Duration;
use stryi_core::block::{Block, BlockHash};
use stryi_core::mempool::MemPool;
use stryi_network::{NetworkCommand, NetworkEvent, PeerId, ServiceRecord, SignedServiceRecord, StryiNetworkManager};
use stryi_storage::StryiStorage;
use tokio::join;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::time::{sleep, Instant};
use tokio_stream::StreamExt;
use tonic::transport::{Endpoint, Server, ServerTlsConfig};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, trace};
use stryi_network::ed25519::Keypair;
use crate::genesis_manager::{GenesisChoice, GenesisManager};
use crate::grpc_services::blockchain_sync_client::BlockchainSyncClient;
use crate::grpc_services::BlockHashList;

/// The main struct representing the Stryi node instance.
/// This node will later integrate networking, consensus, gRPC sync, mempool and mining services.
pub struct StryiChainNode {

    // TODO: Integrate consensus engine, mining loop manager, ...

    /// Mempool object.
    pub(crate) mempool: Arc<RwLock<MemPool>>,
    
    /// The blockchain storage (UTXO set, block storage, undo data etc.)
    pub(crate) storage: Arc<RwLock<StryiStorage>>,
    
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

    /// The genesis manager, responsible for loading/initializing the genesis block and UTXO set.
    pub(crate) genesis_manager: GenesisManager,

    /// Configuration for the gRPC-based synchronization service.
    pub(crate) sync_service_config: StryiSyncServiceConfig,

    /// Configuration for high-level http api service for node's users.
    pub(crate) http_service_config : StryiHttpServiceConfig,
    
    /// The readiness flag used by the gRPC middleware.
    pub(crate) grpc_is_ready: ReadyFlag,

    /// The readiness flag used by the http middleware.
    pub(crate) http_is_ready: ReadyFlag,
}

impl StryiChainNode {
    
    /// connect() is the first step in the node's lifecycle.
    /// It initializes the network manager and connects to the network.
    /// It must be called before any other operations, like synchronization or starting services.
    pub(crate) async fn connect(&mut self) -> Result<(), StryiNodeError> {

        info!("Connecting to the network...");

        let mut mgr = self
            .network_manager
            .take()
            .ok_or_else(|| StryiNodeError::other("network manager not initialized"))?;

        let cmd= mgr.command_sender();
        let events= mgr.subscribe_events();


        // run the network manager in a separate task
        tokio::spawn(async move {
            mgr.run_loop().await
        });

        // Store the command sender and events receiver in the node
        self.net_cmd    = Some(cmd);
        self.net_events = Some(events);

        Ok(())


    }

    // Constants for peer discovery during synchronization
    
    /// How many seconds try to discover peers with gRPC sync service
    const DISCOVERY_TIMEOUT: Duration  = Duration::from_secs(10);
    
    /// How often to poll the network for peers with gRPC sync service
    const DISCOVERY_INTERVAL: Duration = Duration::from_millis(250);



    /// synchronize() is the second step in the node's lifecycle.
    /// It is responsible for synchronizing the node with the network, fetching blocks, transactions,
    /// and other data needed to bring the node up to date.
    pub(crate) async fn synchronize(&mut self) -> Result<(), StryiNodeError> {
        info!("Synchronizing with the network...");


        let net_cmd = self.net_cmd.as_ref().ok_or_else(|| StryiNodeError::other("network not connected"))?;


        let started = Instant::now();
        let mut candidates: Vec<(PeerId, ServiceRecord)> = Vec::new();

        // First, discover peers that offer the gRPC sync service with the compatible version
        // not fail instantly if none found - retry for up to DISCOVERY_TIMEOUT
        loop {
            let (tx, rx) = tokio::sync::oneshot::channel();
            net_cmd.send(NetworkCommand::QueryPeersWithService {
                service: GRPC_SERVICE_TAG.to_owned(),
                respond_to: tx,
            }).await.map_err(|_| StryiNodeError::other("network command channel closed"))?;

            if let Ok(mut v) = rx.await {
                v.retain(|(_, s)| s.version() as usize == self.sync_service_config.protocol_version);
                if !v.is_empty() { candidates = v; break; }
            }

            if started.elapsed() >= Self::DISCOVERY_TIMEOUT { break; }
            sleep(Self::DISCOVERY_INTERVAL).await;
        }

        if candidates.is_empty() {
            return Err(StryiNodeError::other("No compatible gRPC sync service found"));
        }


        let (peer, svc) = candidates[0].clone();

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


        /*
        if !svc.verify_signature(&peer_pubkey) {
            return Err(StryiNodeError::other("TLS certificate signature invalid"));
        }
        */

        info!("Using gRPC sync service at {} (version {})", svc.address(), svc.version());


        debug!("{:#?}", candidates);


        let (grpc_peer_id, grpc_peer_service_info) = candidates
            .into_iter()
            .find(|(_peer, svc)|
                      {
                          trace!("peer's service info: {:?}", svc);
                          trace!("Discovered service version: {}, required: {}", svc.version(), self.sync_service_config.protocol_version);
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
        //  .tls_config(tls_cfg)
        // .map_err(|e| StryiNodeError::other(format!("TLS config error: {e}")))?
            .connect()
            .await
            .map_err(|e| StryiNodeError::other(format!("gRPC dial error: {e}")))?;

        let mut grpc_client = BlockchainSyncClient::new(channel);

        // request the genesis block from the gRPC sync service
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

        // Decide how to proceed with genesis
        match self.genesis_manager.load_saved() {
            // No local genesis: ask user to accept the network one
            None => {
                info!("No local genesis found; prompting user to accept the network genesis.");
                // If user refuses, return an actionable error
                self.genesis_manager
                    .prompt_and_cache_block(&external_genesis_block)?;
                info!("Network genesis accepted and cached.");
            }

            // Local genesis exists
            Some(local_genesis) => {
                info!("Comparing local genesis with the one received from the network...");
                if local_genesis == external_genesis_block {
                    // Match: good scenario
                    info!("[OK]: Genesis blocks are identical.");
                } else {
                    // Mismatch: let user choose which one to use
                    info!("[MISMATCH]: Genesis blocks differ.");
                    let choice = self
                        .genesis_manager
                        .prompt_choose_between(&local_genesis, &external_genesis_block)?;

                    match choice {
                        GenesisChoice::Local => {
                            info!("User chose LOCAL genesis. Proceeding with local chain rules.");
                            // Nothing to change on disk; keep existing file.
                        }
                        GenesisChoice::Network => {
                            info!("User chose NETWORK genesis. Replacing local genesis atomically.");
                            self.genesis_manager.save_block(&external_genesis_block)?;
                        }
                    }
                }
            }
        }


        Ok(())
    }


    /// Starts the node instance and basic services, like mempool, grpc sync server, mining-loop (if set in the config), handles network events
    /// Meant to be called after `connect()` and `synchronize()`.
    pub async fn start_services(self) -> Result<(), Box<dyn std::error::Error>> {

        // Destructure to avoid partial borrows
        // After this - there will be no more "self" itself, but just all the fields/values separated
        let StryiChainNode {
            mempool,
            storage,
            network_manager,
            keypair,
            peer_id,
            services_records,
            tls_identity,
            
            sync_service_config,
            http_service_config,
            grpc_is_ready,
            http_is_ready, ..
        } = self;


        debug_assert!(network_manager.is_none(), "run_loop must be spawned in connect()");

        // clone once per task
        let storage_for_http = Arc::clone(&storage);
        let storage_for_grpc = Arc::clone(&storage);
        
        // http server future
        let http_fut = async {
            
            crate::http::start_http_server(
                storage_for_http,
                http_service_config.clone(),
                mempool.clone(),
                http_is_ready
            ).await
        };
        
        
        // gRPC server future
        // TODO: Make gRPC really use tls based on provider peer's identity keys 
        let grpc_fut = async {

            // Create the sync service instance
            let service_impl = StryiSyncService {
                config: sync_service_config.clone(),
                storage: storage_for_grpc,
            };

            let tonic_identity = tonic::transport::Identity::from_pem(&tls_identity.cert_pem, &tls_identity.key_pem);
            let tls_config = ServerTlsConfig::new().identity(tonic_identity);

            // Wrap the instance in Tonic’s generated server
            let svc = BlockchainSyncServer::new(service_impl);

            // Build the readiness layer middleware.
            let readiness_layer = ReadyGateLayer::new(grpc_is_ready.clone());
            
            // wrap it up
            let svc = ServiceBuilder::new()
                .layer(readiness_layer)
                .service(svc);

            info!("Starting gRPC sync service on {}", &sync_service_config.address);

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
                sync_service_config.protocol_version as u32
            );


            let signed_grpc_record = SignedServiceRecord::sign(keypair.clone(), grpc_record)?;

            info!("Successfully signed node's gRPC service with own keypair!");

            let http_record = ServiceRecord::new(
                http_service_config.address,
                peer_id,
                HTTP_SERVICE_TAG.to_string(),
                http_service_config.api_version
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