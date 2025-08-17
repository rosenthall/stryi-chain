use std::sync::Arc;
use tokio::join;
use tokio::sync::mpsc::Sender;
use tokio::sync::{broadcast, mpsc, RwLock};
use tonic::transport::{Server, ServerTlsConfig};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use stryi_core::block::Block;
use stryi_core::mempool::MemPool;
use stryi_core::transactions::Transaction;
use stryi_network::{BroadcastBlock, NetworkCommand, NetworkEvent, ServiceInfo, StryiNetworkManager};
use stryi_storage::StryiStorage;
use crate::error::StryiNodeError;
use crate::grpc::{StryiSyncService, StryiSyncServiceConfig};
use crate::grpc_services::blockchain_sync_server::BlockchainSyncServer;
use crate::http::StryiHttpServiceConfig;
use crate::middleware::{ReadyFlag, ReadyGateLayer};
use crate::tls::NodeTlsIdentity;

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

    /// A list of services that this node runs
    pub(crate) services_info: Arc<RwLock<Vec<ServiceInfo>>>,

    /// A node's tls identity (based on PeerKey)
    pub(crate) tls_identity: NodeTlsIdentity,


    // Lightweight handles that live after connect().
    /// The command channel for sending network commands to the network manager.
    pub(crate) net_cmd: Option<mpsc::Sender<NetworkCommand>>,
    /// The network events channel, used to receive events from the network manager.
    pub(crate) net_events: Option<broadcast::Receiver<NetworkEvent>>,


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

    /// TODO: Implement synchronization functional for StryiChainNode. Synchronization must be *after* connecting to the network and *before* hosting sync service and processing network events.
    async fn synchronize(mut self) -> Result<(), StryiNodeError> {
        unimplemented!()
    }

    /*
    /// Starts the node instance and basic services, like mempool, grpc sync server, mining-loop (if set in the config), handles network events
    /// Meant to be called after `connect()` and `synchronize()`. 
    pub async fn start_services(self) -> Result<(), Box<dyn std::error::Error>> {

        // Destructure to avoid partial borrows
        // After this - there will be no more "self" itself, but just all the fields/values separated
        let StryiChainNode {
            mempool: _mempool, // TODO: Make mempool mempooling or something
            storage,
            mut network_manager,
            services_info,
            tls_identity,
            net_cmd : net_cmd_,
            sync_service_config,
            http_service_config,
            grpc_is_ready,
            http_is_ready
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
                http_is_ready
            ).await
        };
        
        
        // gRPC server future
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
                .tls_config(tls_config).unwrap()
                // Compress responses
                .layer(CompressionLayer::new())
                // High level logging of requests and responses
                .layer(TraceLayer::new_for_grpc())
                .add_service(svc)
                .serve(sync_service_config.address)
                .await
        };


        // Register gRPC and HTTP service in ServiceInfos
        {
            let grpc_service_info = ServiceInfo::new(
                "grpc-sync".to_owned(),
                sync_service_config.address,
                sync_service_config.protocol_version as u32
            );


            let http_service_info = ServiceInfo::new(
                "http".to_owned(),
                http_service_config.address,
                http_service_config.api_version
            );


            services_info.write().await.push(grpc_service_info);
            services_info.write().await.push(http_service_info);
        }


        // Some more services we need (?)

        // network manager future
        let network_fut = network_manager.run_loop();

        // run all three concurrently
        let (grpc_res, _net_res, _http_res) = join!(grpc_fut, network_fut, http_fut);
        grpc_res?;        // propagate gRPC error if any
        
        Ok(())
    }*/
}