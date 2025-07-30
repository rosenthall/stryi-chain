use std::sync::Arc;
use tokio::join;
use tokio::sync::RwLock;
use tonic::transport::{Server, ServerTlsConfig};
use tower::ServiceBuilder;
use tracing::info;
use stryi_core::mempool::MemPool;
use stryi_network::{ServiceInfo, StryiNetworkManager};
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
    pub(crate) network_manager: StryiNetworkManager,

    /// A list of services that this node runs
    pub(crate) services_info: Arc<RwLock<Vec<ServiceInfo>>>,

    /// A node's tls identity (based on PeerKey)
    pub(crate) tls_identity: NodeTlsIdentity,

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
    /// TODO: Implement `connect` method for StryiChainNode, must be performed as very first step when initializing node.
    async fn connect(mut self) -> Result<(), StryiNodeError> {
        unimplemented!()
    }

    /// TODO: Implement synchronization functional for StryiChainNode. Synchronization must be *after* connecting to the network and *before* hosting sync service and processing network events.
    async fn synchronize(mut self) -> Result<(), StryiNodeError> {
        unimplemented!()
    }

    
    /// Starts the node instance and basic services, like mempool, grpc sync server, mining-loop (if set in the config), handles network events
    /// Meant to be called after `connect()` and `synchronize()`. 
    pub async fn start_services(self) -> Result<(), Box<dyn std::error::Error>> {

        // Destructure to avoid partial borrows
        // After this - there will be no more "self" itself, but just all the fields/values separated
        let StryiChainNode {
            mempool: _mempool, // TODO: Make mempool mempoling or something 
            storage,
            mut network_manager,
            services_info,
            tls_identity,
            sync_service_config,
            http_service_config,
            grpc_is_ready,
            http_is_ready
        } = self;


        // clone once per task
        let storage_for_http = Arc::clone(&storage);
        let storage_for_grpc = Arc::clone(&storage);
        
        // http server future
        let http_fut = async {
            
            crate::http::start_http_server(
                storage_for_http,
                http_service_config, 
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
                .add_service(svc)
                .serve(sync_service_config.address)
                .await
        };


        // Register gRPC service in ServiceInfo's
        {
            let grpc_service_info = ServiceInfo::new("grpc-sync".to_owned(),
                                                     sync_service_config.address,
                                                     1);
            
            services_info.write().await.push(grpc_service_info);
        }
        
        // Some more services we need (?)

        // network manager future
        let network_fut = network_manager.run_loop();

        // run all three concurrently
        let (grpc_res, _net_res, _http_res) = join!(grpc_fut, network_fut, http_fut);
        grpc_res?;        // propagate gRPC error if any
        
        Ok(())
    }
}