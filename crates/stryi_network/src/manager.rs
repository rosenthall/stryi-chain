use std::time::Duration;
use std::sync::Arc;
use futures::StreamExt;
use libp2p::{
    core::upgrade,
    identity::Keypair,
    noise,
    tcp,
    yamux,
    swarm::{Swarm, SwarmEvent, Config as SwarmConfig},
    Transport, PeerId, Multiaddr,
};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{StryiNodeMode, error::StryiNetworkError, behaviour::{StryiBehaviour, StryiBehaviourConfig, StryiEvent}, ServiceStatus, StryiNetworkManagerState};

/// StryiNetworkManager sets up the transport, constructs a swarm using our unified behaviour,
/// and runs the event loop. The swarm is stored as an Option so that the run loop can be spawned
/// and later taken out for shutdown or restart.
pub struct StryiNetworkManager {
    pub(crate) config: StryiNetworkManagerState,
    pub(crate) swarm: Option<Swarm<StryiBehaviour>>,
    pub(crate) status: Arc<RwLock<ServiceStatus>>,
    pub(crate) cancel_token: CancellationToken,
    pub(crate) handle: Option<JoinHandle<()>>,
}

impl StryiNetworkManager {
    /// Creates a new StryiNetworkManager based on the provided configuration.
    pub fn new(config: &StryiNetworkManagerState) -> Result<Self, StryiNetworkError> {
        // Use provided key or generate one.
        let key = config
            .clone()
            .keypair
            .unwrap_or_else(|| Keypair::generate_ed25519());
        let local_peer_id = PeerId::from(key.public());

        // Build the transport (TCP + Noise + Yamux).
        let transport = Self::build_transport(&key)?;

        // Construct StryiBehaviourConfig.
        let behaviour_config = StryiBehaviourConfig {
            keypair: key.clone(),
            enable_server: matches!(config.mode, StryiNodeMode::Server),
            enable_client: matches!(config.mode, StryiNodeMode::Node),
            ping_interval: Duration::from_secs(10),
            ping_timeout: Duration::from_secs(10),
            gossipsub_heartbeat: Duration::from_secs(10),
        };
        let behaviour = StryiBehaviour::new(behaviour_config)?;

        // Create the swarm with default SwarmConfig.
        let swarm_config = SwarmConfig::with_tokio_executor();
        let mut swarm = Swarm::new(transport, behaviour, local_peer_id, swarm_config);

        // Listen on the configured address.
        let listen_addr: Multiaddr = config
            .listen_addr
            .parse()
            .map_err(|e| StryiNetworkError::other(format!("Failed to parse listen address: {e}")))?;
        swarm
            .listen_on(listen_addr)
            .map_err(|e| StryiNetworkError::other(format!("Swarm listen error: {e:?}")))?;

        // If in Node mode and rendezvous server is provided, dial it.
        if let StryiNodeMode::Node = config.mode {
            if let Some(ref srv_addr) = config.rendezvous_server_addr {
                let ma: Multiaddr = srv_addr
                    .parse()
                    .map_err(|e| StryiNetworkError::other(format!("Failed to parse rendezvous server address: {e}")))?;
                swarm
                    .dial(ma)
                    .map_err(|e| StryiNetworkError::other(format!("Swarm dial error: {e:?}")))?;
            }
        }

        Ok(Self {
            config: config.clone(),
            swarm: Some(swarm),
            status: Arc::new(RwLock::new(ServiceStatus::Stopped)),
            cancel_token: CancellationToken::new(),
            handle: None,
        })
    }

    /// Helper function to build a transport (TCP + Noise + Yamux).
    pub(crate) fn build_transport(
        key: &Keypair,
    ) -> Result<libp2p::core::transport::Boxed<(PeerId, libp2p::core::muxing::StreamMuxerBox)>, StryiNetworkError> {
        let noise_config = noise::Config::new(key)
            .map_err(StryiNetworkError::NoiseConfigError)?;
        let tcp_transport = tcp::tokio::Transport::new(tcp::Config::default());
        let transport = tcp_transport
            .upgrade(upgrade::Version::V1)
            .authenticate(noise_config)
            .multiplex(yamux::Config::default())
            .boxed();

        Ok(transport)
    }

    /// Internal run loop that processes swarm events and exits gracefully when the cancellation token is triggered.
    pub(crate) async fn run_loop(&mut self, token: CancellationToken) {
        // We require a mutable reference to the swarm. Since self.swarm is an Option,
        // we take it out and then later put it back if needed.
        let mut swarm = self.swarm.take().expect("Swarm should be available in run_loop");
        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    tracing::info!("Cancellation requested, exiting run_loop");
                    break;
                }
                event = swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr { address, .. } => {
                            tracing::info!("Listening on {}", address);
                        }
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            tracing::info!("Connected to {}", peer_id);
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            tracing::info!("Disconnected from {}", peer_id);
                        }
                        SwarmEvent::Behaviour(event) => {
                            match event {
                                StryiEvent::Gossipsub(e) => tracing::info!("Gossipsub event: {:?}", e),
                                StryiEvent::Ping(e) => tracing::info!("Ping event: {:?}", e),
                                StryiEvent::Identify(e) => tracing::info!("Identify event: {:?}", e),
                                StryiEvent::RzvServer(e) => tracing::info!("Rendezvous Server event: {:?}", e),
                                StryiEvent::RzvClient(e) => tracing::info!("Rendezvous Client event: {:?}", e),
                            }
                        }
                        other => {
                            tracing::debug!("Other event: {:?}", other);
                        }
                    }
                }
            }
        }
        tracing::info!("run_loop terminated gracefully.");
        // Put the swarm back so it can be used for future commands.
        self.swarm = Some(swarm);
    }
}