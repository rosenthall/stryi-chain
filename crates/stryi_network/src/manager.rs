use crate::mempool::{MempoolResponse, MempoolSyncBehaviour};
use crate::services::{ServiceInfo, ServicesResponse};
use crate::{
    NetworkCommand, NetworkEvent, RendezvousMode, StryiNetworkManagerConfig, behaviour,
    behaviour::{StryiBehaviour, StryiBehaviourConfig, StryiEvent},
    error::StryiNetworkError,
};
use libp2p::core::transport::Boxed;
use libp2p::gossipsub::IdentTopic;
use libp2p::request_response::{
    InboundRequestId, ProtocolSupport, ResponseChannel,
};
use libp2p::{
    Multiaddr, PeerId, Transport,
    core::upgrade,
    gossipsub,
    identity::Keypair,
    noise, ping, request_response,
    swarm::{Config as SwarmConfig, Swarm},
    tcp, yamux,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use futures::StreamExt;
use stryi_core::mempool::MemPool;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, trace, warn};

/// StryiNetworkManager sets up the transport, constructs a swarm using our unified StryiBehaviour,
/// and runs the event loop.
/// Provides high-level communication layer with network via channels and messaging such as
/// - `command_tx` : Channel for communicating with entire network, allows performing operations like publish blocks/transactions, dial with specific node, etc.
/// - `event_tx` : Channel for receiving `NetworkEvents` from StryiNetworkManager
pub struct StryiNetworkManager {
    /// Configuration for entire StryiNetworkManager instance, defines addresses, keypair, rendezvous mode, etc.
    pub(crate) config: StryiNetworkManagerConfig,

    /// Swarm with specified behaviour (using StryiBehaviour)
    pub(crate) swarm: Arc<Mutex<Swarm<StryiBehaviour>>>,

    /// Arc reference to mempool object
    pub(crate) mempool: Arc<RwLock<MemPool>>,

    /// Channel to perform operation in network like sending blocks, transactions, dialing a connections, etc.
    pub(crate) command_tx: mpsc::Sender<NetworkCommand>,

    /// Receiver side for `command_tx`, must be used in the run loop.
    pub(crate) command_rx: mpsc::Receiver<NetworkCommand>,

    // Event channel: network manager broadcasts events (e.g., peer events) to subscribers
    pub(crate) event_tx: broadcast::Sender<NetworkEvent>,

    /// Thread-safe, mutable registry of this node’s active services.
    /// Wrapped in an `RwLock` to allow concurrent reads and real-time updates
    /// (e.g. when a service starts, stops, or changes its listening port).
    pub(crate) services_info: Arc<RwLock<Vec<ServiceInfo>>>,

    /// Connected peers tracking TODO : Actually track peers
    pub(crate) connected_peers: Arc<RwLock<HashMap<PeerId, PeerInfo>>>,

    /// Peer id of this network manager
    pub(crate) peer_id: PeerId,

    /// Cancellation token for graceful shutdown of the run loop
    cancel_token: CancellationToken,
}

#[derive(Clone, Debug)]
pub struct PeerInfo {
    /// Time the connection was established, if known.
    pub established_at: Option<SystemTime>,
    /// Most recent time the peer was seen or updated.
    pub last_seen: Option<SystemTime>,
    /// All known addresses for this peer. TODO: Is storing more than 1 address of single peer is necessary for design?
    pub addresses: Vec<Multiaddr>,
    /// gRPC sync server address, if available.
    pub grpc_sync_server_address: Option<SocketAddr>,

    /// Number of consecutive ping failures for this peer.
    /// This is used to detect unresponsive peers and potentially disconnect them.
    pub consecutive_ping_failures: usize,
    
}

// hard‑coded topic names that every node must agree on
const BLOCKS_TOPIC_NAME: &str = "stryichain-blocks";
const TRANSACTIONS_TOPIC_NAME: &str = "stryichain-txs";

impl StryiNetworkManager {
    /// Creates a new StryiNetworkManager based on the provided configuration, Arc-ed mempool, and cancellation_token
    pub fn new(
        config: &StryiNetworkManagerConfig,
        mempool: Arc<RwLock<MemPool>>,
        services_info: Arc<RwLock<Vec<ServiceInfo>>>,
        cancel_token: CancellationToken,
    ) -> Result<Self, StryiNetworkError> {
        // Use provided key or generate one.
        let key = config.clone().keypair;
        let local_peer_id = PeerId::from(key.public());

        // Build the transport (TCP + Noise + Yamux).
        let transport = Self::build_transport(&key)?;

        // Construct StryiBehaviourConfig.
        let behaviour_config = StryiBehaviourConfig {
            enable_rendezvous_server: matches!(config.rendezvous_mode, RendezvousMode::Server),
            enable_rendezvous_client: matches!(config.rendezvous_mode, RendezvousMode::Client),
            ping_interval: Duration::from_secs(10),
            ping_timeout: Duration::from_secs(10),
            gossipsub_heartbeat: Duration::from_secs(10),
        };

        let behaviour = StryiBehaviour::new(behaviour_config, &key, &config.version)?;

        // Create the swarm with default SwarmConfig.
        let swarm_config = SwarmConfig::with_tokio_executor();
        let mut swarm = Swarm::new(transport, behaviour, local_peer_id, swarm_config);

        // Listen on the configured address.
        let listen_addr: Multiaddr = config.listen_addr.parse().map_err(|e| {
            StryiNetworkError::other(format!("Failed to parse listen address: {e}"))
        })?;

        swarm
            .listen_on(listen_addr)
            .map_err(|e| StryiNetworkError::other(format!("Swarm listen error: {e:?}")))?;

        info!(
            mode = ?config.rendezvous_mode,
            listen = %config.listen_addr,
            dial   = ?config.rendezvous_server_addr,
            %local_peer_id,
            "Initializing StryiNetworkManager"
        );

        // Client mode: dial rendezvous server if provided
        if matches!(config.rendezvous_mode, RendezvousMode::Client) {
            if let Some(ref srv_addr) = config.rendezvous_server_addr {
                if !srv_addr.is_empty() {
                    // avoid trivially dialing our own listen addr string-for-string
                    if srv_addr == &config.listen_addr {
                        warn!(
                            "Rendezvous server address equals our listen addr ({}). Skipping dial.",
                            srv_addr
                        );
                    } else {
                        match srv_addr.parse::<Multiaddr>() {
                            Ok(ma) => {
                                info!("Dialing rendezvous server at {}", srv_addr);
                                if let Err(e) = swarm.dial(ma) {
                                    tracing::warn!("Dial to rendezvous server failed: {e:?}");
                                }
                            }
                            Err(e) => {
                                warn!("Invalid rendezvous address '{}': {e:?}", srv_addr)
                            }
                        }
                    }
                } else {
                    warn!("Client mode but rendezvous_server_addr is an empty string");
                }
            } else {
                warn!("Client mode but no rendezvous_server_addr configured");
            }
        }
        // Create channels for commands
        let (command_tx, command_rx) = mpsc::channel::<NetworkCommand>(32);
        // And for events
        let (event_tx, _) = broadcast::channel::<NetworkEvent>(32);

        Ok(Self {
            config: config.to_owned(),
            swarm: Arc::new(Mutex::new(swarm)),
            mempool,
            command_tx,
            command_rx,
            event_tx,
            services_info,
            connected_peers: Arc::new(Default::default()),
            peer_id: local_peer_id,
            cancel_token,
        })
    }

    /// Get own peer id
    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Returns a new receiver that can be used by callers to listen for network events.
    pub fn subscribe_events(&self) -> broadcast::Receiver<NetworkEvent> {
        self.event_tx.subscribe()
    }

    /// Returns a sender that can be used to send commands to the network manager.
    pub fn command_sender(&self) -> mpsc::Sender<NetworkCommand> {
        self.command_tx.clone()
    }

    /// Helper function to build a transport (TCP + Noise + Yamux).
    pub(crate) fn build_transport(
        key: &Keypair,
    ) -> Result<Boxed<(PeerId, libp2p::core::muxing::StreamMuxerBox)>, StryiNetworkError> {
        let noise_config = noise::Config::new(key).map_err(StryiNetworkError::NoiseConfigError)?;
        let tcp_transport = tcp::tokio::Transport::new(tcp::Config::default());
        let transport = tcp_transport
            .upgrade(upgrade::Version::V1)
            .authenticate(noise_config)
            .multiplex(yamux::Config::default())
            .boxed();

        Ok(transport)
    }

    pub async fn run_loop(&mut self) {
        let mut swarm = self.swarm.lock().await;

        // Subscribe on gossipsub topics for blocks and transactions
        let transactions_topic = IdentTopic::new(TRANSACTIONS_TOPIC_NAME);
        let blocks_topic = IdentTopic::new(BLOCKS_TOPIC_NAME);

        // TODO: Handle somehow gossipsub subscription error
        swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&transactions_topic)
            .unwrap();
        swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&blocks_topic)
            .unwrap();

        loop {
            tokio::select! {

                // --- Cancellation token ---
                _ = self.cancel_token.cancelled() => {
                    info!("Cancellation requested, exiting StryiNetworkManager run loop.");
                    break;
                }

                // --- Network commands ---

                command = self.command_rx.recv() => {
                    // TODO: Implement NetworkCommands handler in network-manager loop
                    info!("Received command : {command:?}");
                }

                // --- libp2p events ---
                event = swarm.select_next_some() => {
                    self.process_event(&mut swarm, event).await;
                }

            }
            info!("StryiNetworkManager run loop terminated gracefully.");
        }
    }

}
