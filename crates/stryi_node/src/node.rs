use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::transport::{Error, Server};

use stryi_storage::StryiStorage;
use crate::grpc::{StryiSyncService, StryiSyncServiceConfig};
use crate::grpc_services::blockchain_sync_server::BlockchainSyncServer;

/// The main struct representing the Stryi node instance.
/// This node will later integrate networking, consensus and gRPC services.
pub struct StryiChainNode {

    // TODO: Integrate network manager, consensus engine and sync service into this node
    // pub(crate) network_manager: StryiNetworkManager,
    // pub(crate) sync_service: StryiSyncService<StryiStorage>,
    // pub(crate) consensus_engine: StryiConsensusEngine<StryiStorage>,

    /// The blockchain storage (UTXO set, block storage, undo data etc.)
    pub(crate) storage: Arc<RwLock<StryiStorage>>,

    /// Configuration for the gRPC-based synchronization service.
    pub(crate) stryi_sync_service_config: StryiSyncServiceConfig,
}

impl StryiChainNode {

    /// Starts the node instance.
    ///
    /// At the current stage, this only starts the gRPC sync server.
    /// In the future, this will also start the consensus engine and networking layer.
    pub async fn start(self) -> Result<(), Box<dyn std::error::Error>> {
        self.start_sync_service().await?;
        Ok(())
    }

    /// Starts the gRPC synchronization service.
    ///
    /// This exposes block and chain synchronization API via gRPC.
    async fn start_sync_service(&self) -> Result<(), Error> {
        // Clone configuration (cheap, since StryiSyncServiceConfig is probably small)
        let config = self.stryi_sync_service_config.clone();

        // Create the sync service implementation
        let service_impl = StryiSyncService {
            config: config.clone(),
            storage: self.storage.clone(),
        };

        // Wrap the implementation in Tonic’s generated server
        let svc = BlockchainSyncServer::new(service_impl);

        // Build the socket address for the gRPC server
        let addr = SocketAddr::new("127.0.0.1".parse().unwrap(), config.port as u16);

        // Start serving the sync service on the configured port
        Server::builder()
            .add_service(svc)
            .serve(addr)
            .await
    }
}
