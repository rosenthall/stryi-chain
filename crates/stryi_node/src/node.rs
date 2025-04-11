use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::join;
use tokio_stream::StreamExt;
use tonic::transport::Server;
use tower::ServiceBuilder;
use tracing::info;
use stryi_network::{NetworkEvent, NetworkService, ServiceStatus, StryiNetworkManager};
use stryi_storage::StryiStorage;
use crate::error::StryiNodeError;
use crate::grpc::{ReadinessMiddlewareLayer, StryiSyncService, StryiSyncServiceConfig};
use crate::grpc_services::blockchain_sync_server::BlockchainSyncServer;

/// The main struct representing the Stryi node instance.
/// This node will later integrate networking, consensus, gRPC sync, mempool and mining services.
pub struct StryiChainNode {

    // TODO: Integrate consensus engine, mempool, mining loop manager, ...

    /// The blockchain storage (UTXO set, block storage, undo data etc.)
    pub(crate) storage: Arc<RwLock<StryiStorage>>,
    
    // The blockchain's p2p layer, instance of StryiNetworkManager that allows to communicate with other nodes
    pub(crate) network_manager: StryiNetworkManager,
    
    /// Configuration for the gRPC-based synchronization service.
    pub(crate) sync_service_config: StryiSyncServiceConfig,
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


    /// Starts the node instance and basic services, like mempool, grpc sync server, mining-loop (if set in the config), handles network events,  
    /// Meant to be called after `connect()` and `synchronize()`. 
    pub async fn start_services(self) -> Result<(), Box<dyn std::error::Error>> {

        // Destructure to avoid partial borrows
        // After this - there will be no more "self" itself, but just all the fields/values separated
        let StryiChainNode {
            storage,
            mut network_manager,
            sync_service_config,
        } = self;

        // gRPC server future
        let grpc_fut = async {

            // Create the sync service instance
            let service_impl = StryiSyncService {
                config: sync_service_config.clone(),
                storage,
            };

            // Wrap the instance in Tonic’s generated server
            let svc = BlockchainSyncServer::new(service_impl);


            // Build the readiness layer middleware.
            let readiness_layer = ReadinessMiddlewareLayer::default();


            info!("Starting gRPC sync service on {}", &sync_service_config.address);

            // Start serving the sync service on the configured port
            Server::builder()
                .layer(
                    ServiceBuilder::new()
                        .layer(readiness_layer)
                )
                .add_service(svc)
                .serve(sync_service_config.address)
                .await
        };

        // network manager future
        let network_fut = network_manager.start();

        // run both concurrently
        let (grpc_res, net_res) = join!(grpc_fut, network_fut);
        grpc_res?;
        net_res?;

        Ok(())
    }
}