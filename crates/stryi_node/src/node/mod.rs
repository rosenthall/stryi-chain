use crate::bootstrap::GenesisBootstrap;
use crate::error::StryiNodeError;
use crate::grpc::StryiSyncServiceConfig;
use crate::http::StryiHttpServiceConfig;
use crate::middleware::ready::ReadyFlag;
use crate::node::event_loop::EventLoop;
use crate::node::miner_bridge::MinerBridge;
use crate::tls::NodeTlsIdentity;
use multiaddr::Multiaddr;
use std::sync::Arc;
use stryi_core::block::BlockHash;
use stryi_core::consensus::StryiConsensusEngine;
use stryi_core::mempool::MemPool;
use stryi_network::{NetworkCommand, NetworkEvent, StryiNetworkManager};
use stryi_storage::StryiStorage;
use tokio::sync::broadcast::Sender;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Simple abstraction for ConsensusEngine <-> Miner communication.
pub(crate) mod miner_bridge;

/// Cool implementation of the algorithm that finds the Last Common Ancestor.
mod lca;

/// Provides high-level helpers for interacting with remote peers
mod remote_peer;

/// Node's synchronization primitives
pub(crate) mod sync;

/// Helpers for performing Initial Block Download and mass applying blocks.
mod ibd;

/// **Main** event loop of the node
mod event_loop;

/// The first stage of node initialization.
/// At this point this node is not yet connected and has no `ConsensusEngine` initialized
pub struct StryiChainNode {
    pub(crate) storage: Arc<RwLock<StryiStorage>>,
    pub(crate) mempool: Arc<RwLock<MemPool>>,
    pub(crate) network_manager: StryiNetworkManager,
    pub(crate) genesis_bootstrap: GenesisBootstrap,
    pub(crate) sync_service_config: StryiSyncServiceConfig,
}

/// Node after connect().
/// Has live network channels, ready for sync.
pub struct ConnectedNode {
    pub(crate) storage: Arc<RwLock<StryiStorage>>,
    pub(crate) mempool: Arc<RwLock<MemPool>>,
    pub(crate) net_cmd: mpsc::Sender<NetworkCommand>,
    pub(crate) net_events: broadcast::Receiver<NetworkEvent>,
    pub(crate) genesis_bootstrap: GenesisBootstrap,
    pub(crate) sync_service_config: StryiSyncServiceConfig,
}

/// Node after synchronize() or bootstrap(). Has a consensus engine ready to go.
pub struct SyncedNode {
    pub(crate) storage: Arc<RwLock<StryiStorage>>,
    pub(crate) mempool: Arc<RwLock<MemPool>>,
    pub(crate) net_cmd: mpsc::Sender<NetworkCommand>,
    pub(crate) net_events: broadcast::Receiver<NetworkEvent>,
    pub(crate) consensus_engine: Arc<Mutex<StryiConsensusEngine<StryiStorage>>>,
    pub(crate) sync_service_config: StryiSyncServiceConfig,
}

/// Everything the event loop needs after connect/sync is finished.
/// This includes:
/// - [`ReadyFlag`]'s for both http and grpc APIs,
/// - [`MinerBridge`] for interactions with the local miner,
/// - Channels from the [`StryiNetworkManager`] so EventLoop can receive and send messages,
/// - [`CancellationToken`] for the entire event loop,
/// - Configuration such as advertised addresses and TLS identity.
pub(crate) struct EventLoopContext {
    pub tip_updates_sender: Sender<BlockHash>,
    pub miner_bridge: Option<MinerBridge>,
    pub grpc_is_ready: ReadyFlag,
    pub http_is_ready: ReadyFlag,
    pub cancel_token: CancellationToken,

    pub tls_identity: NodeTlsIdentity,
    pub http_service_config: StryiHttpServiceConfig,
    pub http_advertise_address: Multiaddr,
    pub grpc_advertise_address: Multiaddr,
}

impl StryiChainNode {
    pub fn new(
        storage: Arc<RwLock<StryiStorage>>,
        mempool: Arc<RwLock<MemPool>>,
        network_manager: StryiNetworkManager,
        genesis_bootstrap: GenesisBootstrap,
        sync_service_config: StryiSyncServiceConfig,
    ) -> Self {
        Self {
            storage,
            mempool,
            network_manager,
            genesis_bootstrap,
            sync_service_config,
        }
    }

    /// Spawns the network loop, returns a ConnectedNode with live channels.
    pub async fn connect(self) -> Result<ConnectedNode, StryiNodeError> {
        info!("Connecting to the network...");

        let mut mgr = self.network_manager;
        let cmd = mgr.command_sender();
        let events = mgr.subscribe_events();

        tokio::spawn(async move { mgr.run_loop().await });

        Ok(ConnectedNode {
            storage: self.storage,
            mempool: self.mempool,
            net_cmd: cmd,
            net_events: events,
            genesis_bootstrap: self.genesis_bootstrap,
            sync_service_config: self.sync_service_config,
        })
    }
}

impl SyncedNode {
    /// builds the event loop from the synced node and additional contexts
    pub fn into_event_loop(self, ctx: EventLoopContext) -> EventLoop {
        EventLoop {
            storage: self.storage,
            mempool: self.mempool,
            consensus_engine: self.consensus_engine,
            sync_service_config: self.sync_service_config,
            tip_updates_sender: ctx.tip_updates_sender,
            miner_bridge: ctx.miner_bridge,
            tls_identity: ctx.tls_identity,
            http_service_config: ctx.http_service_config,
            grpc_is_ready: ctx.grpc_is_ready,
            http_is_ready: ctx.http_is_ready,
            http_advertise_address: ctx.http_advertise_address,
            grpc_advertise_address: ctx.grpc_advertise_address,
            cmd_sender: self.net_cmd,
            net_events: self.net_events,
            cancellation_token: ctx.cancel_token,
        }
    }
}
