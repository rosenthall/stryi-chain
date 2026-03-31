use crate::mempool::MempoolRequest;
use crate::peer::PeerInfo;
use crate::services::{
    ServiceRecord, ServicesInfoRequest, SignedServiceRecord, filter_verified_records,
};
use crate::{
    NetworkCommand, NetworkEvent, RendezvousMode, StryiNetworkManagerConfig,
    behaviour::StryiBehaviour, error::StryiNetworkError,
};
use bincode::config::standard;
use futures::StreamExt;
use libp2p::core::transport::Boxed;
use libp2p::gossipsub::{IdentTopic, PublishError};
use libp2p::request_response::OutboundRequestId;
use libp2p::{
    Multiaddr, PeerId, Transport,
    core::upgrade,
    dns,
    identity::Keypair,
    noise,
    swarm::{Config as SwarmConfig, Swarm},
    tcp, yamux,
};
use rand::prelude::IteratorRandom;
use std::collections::HashMap;
use std::sync::Arc;
use stryi_core::mempool::{MemPool, MemPoolSyncData};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Coordinates the node's libp2p networking state and event loop.
/// Processes inbound `NetworkCommand`s and broadcasts resulting `NetworkEvent`s.
pub struct StryiNetworkManager {
    /// Configuration for the entire StryiNetworkManager instance, defines addresses, keypair, rendezvous mode, etc.
    pub(crate) config: StryiNetworkManagerConfig,

    pub(crate) peer_id: PeerId,
    pub(crate) swarm: Arc<Mutex<Swarm<StryiBehaviour>>>,
    pub(crate) mempool: Arc<RwLock<MemPool>>,

    // Inbound command channel consumed by the event loop.
    pub(crate) command_tx: mpsc::Sender<NetworkCommand>,

    /// Receiver side for `command_tx`, used in the run loop.
    pub(crate) command_rx: mpsc::Receiver<NetworkCommand>,

    // Event channel. network manager broadcasts events (e.g., peer events) to subscribers
    pub(crate) event_tx: broadcast::Sender<NetworkEvent>,

    /// Thread-safe, mutable registry of this node's active services.
    /// Wrapped in an `RwLock` to allow concurrent reads and real-time updates
    /// (for instance, when a service starts, stops, or changes its listening port).
    pub(crate) own_services_registry: Arc<RwLock<Vec<SignedServiceRecord>>>,

    /// Connected peers tracking
    pub(crate) connected_peers: Arc<RwLock<HashMap<PeerId, PeerInfo>>>,

    cancel_token: CancellationToken,
}

// hard-coded topic names that every node must agree on
pub const BLOCKS_TOPIC_NAME: &str = "stryichain-blocks";
pub const TRANSACTIONS_TOPIC_NAME: &str = "stryichain-txs";
pub const TIPS_TOPIC_NAME: &str = "stryichain-tips";

impl StryiNetworkManager {
    /// Creates a new StryiNetworkManager based on the provided configuration,
    /// shared mempool instance, and global cancellation_token
    pub fn new(
        config: &StryiNetworkManagerConfig,
        mempool: Arc<RwLock<MemPool>>,
        cancel_token: CancellationToken,
    ) -> Result<Self, StryiNetworkError> {
        let key = config.clone().keypair;
        let local_peer_id = PeerId::from(key.public());
        info!("local_peer_id={}", local_peer_id);

        // Build the transport (which is basically TCP + Noise + yamux).
        let transport = Self::build_transport(&key)?;

        let behaviour = StryiBehaviour::new(
            config.stryi_behaviour_config.clone(),
            config.rendezvous_mode.clone(),
            &key,
        )?;

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

        // In Client mode we dial rendezvous server if provided
        if matches!(config.rendezvous_mode, RendezvousMode::Client) {
            if let Some(ref srv_addr) = config.rendezvous_server_addr {
                if !srv_addr.is_empty() {
                    // avoid trivially dialing our own listen addr
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
                                    warn!("Dial to rendezvous server failed: {e:?}");
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
        let (command_tx, command_rx) = mpsc::channel::<NetworkCommand>(32);
        let (event_tx, _) = broadcast::channel::<NetworkEvent>(32);
        let own_services_registry = Arc::new(RwLock::new(Vec::new()));

        Ok(Self {
            config: config.to_owned(),
            swarm: Arc::new(Mutex::new(swarm)),
            mempool,
            command_tx,
            command_rx,
            event_tx,
            own_services_registry,
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

    /// Returns a random peer that exposes a service of the requested `kind`.
    /// The helper verifies each signed record (signature / owner) on-the-fly.
    /// `None` is returned if no peer currently matches.
    pub async fn random_peer_with_service(&self, kind: &str) -> Option<(PeerId, ServiceRecord)> {
        let peers = self.connected_peers.read().await;
        let mut rng = rand::rng();

        peers
            .iter()
            .filter_map(|(peer_id, info)| {
                let pk = info.public_key.as_ref()?; // identify not finished -> skip

                // Convert the *signed* list into verified `ServiceRecord`s.
                let verified = filter_verified_records(
                    info.services.clone(),
                    &pk.clone().try_into_ed25519().unwrap(),
                );

                // Pick the first record that matches `kind`.
                verified
                    .into_iter()
                    .find(|s| s.kind() == kind)
                    .map(|svc| (*peer_id, svc))
            })
            .choose(&mut rng)
    }

    /// Returns generic keypair for this network manager.
    pub fn get_keypair(&self) -> Keypair {
        self.config.keypair.clone()
    }

    /// Returns this peer's keypair as an ed25519 keypair instead of a generic keypair.
    pub fn get_keypair_ed25519(&self) -> libp2p::identity::ed25519::Keypair {
        self.config
            .keypair
            .clone()
            .try_into_ed25519()
            .expect("local node must use an ed25519 key")
    }

    /// Helper function to build a transport (TCP + DNS + Noise + Yamux).
    pub(crate) fn build_transport(
        key: &Keypair,
    ) -> Result<Boxed<(PeerId, libp2p::core::muxing::StreamMuxerBox)>, StryiNetworkError> {
        let noise_config = noise::Config::new(key).map_err(StryiNetworkError::NoiseConfigError)?;

        let tcp_transport = tcp::tokio::Transport::new(tcp::Config::default());

        let dns_tcp_transport = dns::tokio::Transport::system(tcp_transport)
            .map_err(StryiNetworkError::DnsConfigError)?;

        let transport = dns_tcp_transport
            .upgrade(upgrade::Version::V1Lazy)
            .authenticate(noise_config)
            .multiplex(yamux::Config::default())
            .boxed();

        Ok(transport)
    }

    /// Runs the main event loop of the StryiNetworkManager.
    /// This loop handles incoming network events, processes commands, and manages subscriptions.
    pub async fn run_loop(&mut self) {
        let mut swarm = self.swarm.lock().await;

        // Subscribe on gossipsub topics for blocks, transactions, and chain tips
        let transactions_topic = IdentTopic::new(TRANSACTIONS_TOPIC_NAME);
        let blocks_topic = IdentTopic::new(BLOCKS_TOPIC_NAME);
        let tips_topic = IdentTopic::new(TIPS_TOPIC_NAME);

        // Pending mempool fetch requests, keyed by outbound request ID.
        // Local to the run loop so we avoid borrow conflicts with `self`.
        let mut pending_mempool_fetches: HashMap<
            OutboundRequestId,
            tokio::sync::oneshot::Sender<Result<MemPoolSyncData, StryiNetworkError>>,
        > = HashMap::new();

        // TODO:  return an initialization error if any gossipsub topic subscription fails.
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
        swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&tips_topic)
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
                    match command {
                        // -- QueryPeersWithService command --
                        Some(NetworkCommand::QueryPeersWithService { service, respond_to }) => {
                            debug!("NetworkManager: Got QueryPeersWithService command.");

                            // Read the current peer map atomically
                            let mut discovered_services: Vec<(PeerId, ServiceRecord)> = {
                                let guard = self.connected_peers.read().await;
                                let mut out = Vec::new();

                                for (peer_id, info) in guard.iter() {
                                    // We can verify only if the peer has already sent its public key.
                                    let Some(pk_generic) = &info.public_key else { continue };

                                    // Convert generic `PublicKey` -> concrete ed25519 key expected by the helper.
                                    let Ok(pk_ed) = pk_generic.clone().try_into_ed25519() else { continue };

                                    // Verify signatures, decode `ServiceRecord`s.
                                    let verified = filter_verified_records(info.services.clone(), &pk_ed);

                                    // Retain only the requested kind.
                                    for svc in verified.into_iter().filter(|s| s.kind() == service) {
                                        out.push((*peer_id, svc));
                                    }
                                }
                                out
                            };


                            // Add services of this very peer
                            let own_signed = self.own_services_registry.read().await.clone();
                            let own_public_key  = self.get_keypair_ed25519().public();
                            let own_verified = filter_verified_records(own_signed, &own_public_key);

                            for svc in own_verified.into_iter().filter(|s| s.kind() == service) {
                                discovered_services.push((self.peer_id, svc));
                            }

                            info!(
                                "NetworkManager: Found {} peers with service '{}'.",
                                discovered_services.len(),
                                service
                            );

                            discovered_services.iter().for_each(|(p, s)| {
                                debug!(" <^-^> Peer {}: {:?}", p, s);
                            });


                            // reply (ignore if receiver is gone)
                            let _ = respond_to.send(discovered_services);
                        }

                        // -- AddService command --
                        Some(NetworkCommand::AddService { service, respond_to }) => {
                            let own_pk_ed = self.get_keypair_ed25519();

                            debug!("NetworkManager: Got AddService command, adding service to advertising list: {}", service.kind());

                            match SignedServiceRecord::sign(own_pk_ed, service) {
                                Ok(signed_service) => {
                                    let own_services = {
                                        let mut registry = self.own_services_registry.write().await;
                                        registry.push(signed_service);
                                        registry.clone()
                                    };
                                    let connected_peers = {
                                        let peers = self.connected_peers.read().await;
                                        peers.keys().copied().collect::<Vec<_>>()
                                    };

                                    let pushed = Self::push_services_to_peers(
                                        swarm.behaviour_mut(),
                                        connected_peers,
                                        own_services,
                                    );
                                    debug!(
                                        "Queued service advertisement push for {} peers",
                                        pushed
                                    );

                                    let _ = respond_to.send(Ok(()));
                                }
                                Err(e) => {
                                    warn!("Failed to sign service record: {}", e);
                                    let _ = respond_to.send(Err(e));
                                }
                            }
                        }


                        // -- QueryPeerPublicKey command --
                        Some(NetworkCommand::QueryPeerPublicKey { peer, respond_to }) => {
                            debug!("NetworkManager: Got QueryPeerPublicKey command.");

                            // Look up the peer's public key
                            let public_key = {
                                let guard = self.connected_peers.read().await;
                                guard.get(&peer).and_then(|info| info.public_key.clone())
                            };

                            // Reply (ignore if receiver is gone)
                            let _ = respond_to.send(public_key);
                        }

                        // -- PublishBlock command --
                        Some(NetworkCommand::PublishBlock { block: broadcast_block, respond_to }) => {
                            debug!("NetworkManager: Got PublishBlock command.");
                            let result = match bincode::serde::encode_to_vec(&broadcast_block, standard()) {
                                Ok(encoded) => {
                                    let topic = IdentTopic::new(BLOCKS_TOPIC_NAME);
                                    match swarm.behaviour_mut().gossipsub.publish(topic, encoded) {
                                        Ok(_) => Ok(()),
                                        Err(PublishError::InsufficientPeers) => {
                                            let connected_peers = self.connected_peers.read().await.len();
                                            if connected_peers == 0 {
                                                debug!("Skipping block publish: no gossipsub peers connected yet");
                                                Ok(())
                                            } else {
                                                warn!(
                                                    "Failed to publish block to gossipsub with InsufficientPeers despite {} connected peer(s)",
                                                    connected_peers
                                                );
                                                Err(PublishError::InsufficientPeers.into())
                                            }
                                        }
                                        Err(e) => {
                                            warn!("Failed to publish block to gossipsub: {e:?}");
                                            Err(e.into())
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to encode block for gossipsub: {e:?}");
                                    Err(StryiNetworkError::other(format!("encode block: {e:?}")))
                                }
                            };
                            let _ = respond_to.send(result);
                        }

                        // -- PublishTransaction command --
                        Some(NetworkCommand::PublishTransaction { transaction: tx, respond_to }) => {
                            debug!("NetworkManager: Got PublishTransaction command.");
                            let result = match bincode::serde::encode_to_vec(&tx, standard()) {
                                Ok(encoded) => {
                                    let topic = IdentTopic::new(TRANSACTIONS_TOPIC_NAME);
                                    match swarm.behaviour_mut().gossipsub.publish(topic, encoded) {
                                        Ok(_) => Ok(()),
                                        Err(PublishError::InsufficientPeers) => {
                                            let connected_peers = self.connected_peers.read().await.len();
                                            if connected_peers == 0 {
                                                debug!("Skipping transaction publish: no gossipsub peers connected yet");
                                                Ok(())
                                            } else {
                                                warn!(
                                                    "Failed to publish transaction to gossipsub with InsufficientPeers despite {} connected peer(s)",
                                                    connected_peers
                                                );
                                                Err(PublishError::InsufficientPeers.into())
                                            }
                                        }
                                        Err(e) => {
                                            warn!("Failed to publish transaction to gossipsub: {e:?}");
                                            Err(e.into())
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to encode transaction for gossipsub: {e:?}");
                                    Err(StryiNetworkError::other(format!("encode transaction: {e:?}")))
                                }
                            };
                            let _ = respond_to.send(result);
                        }

                        // -- PublishChainTip command --
                        Some(NetworkCommand::PublishChainTip { announcement, respond_to }) => {
                            debug!("NetworkManager: Got PublishChainTip command.");
                            let result = match bincode::serde::encode_to_vec(&announcement, standard()) {
                                Ok(encoded) => {
                                    let topic = IdentTopic::new(TIPS_TOPIC_NAME);
                                    if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, encoded) {
                                        match &e {
                                            PublishError::InsufficientPeers => {
                                                debug!("Skipping chain tip publish: no gossipsub peers connected yet (we are bootstrapping!");
                                            }
                                            _ => warn!("Failed to publish chain tip to gossipsub: {e:?}"),
                                        }
                                        Err(e.into())
                                    } else {
                                        Ok(())
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to encode chain tip for gossipsub: {e:?}");
                                    Err(StryiNetworkError::other(format!("encode chain tip: {e:?}")))
                                }
                            };
                            let _ = respond_to.send(result);
                        }

                        // -- FetchMempoolState command --
                        Some(NetworkCommand::FetchMempoolState { respond_to }) => {
                            debug!("NetworkManager: Got FetchMempoolState command.");

                            // Pick a random connected peer
                            let random_peer = {
                                let guard = self.connected_peers.read().await;
                                let mut rng = rand::rng();
                                guard.keys().choose(&mut rng).copied()
                            };

                            match random_peer {
                                Some(peer_id) => {
                                    let request_id = swarm
                                        .behaviour_mut()
                                        .mempool_sync
                                        .send_request(&peer_id, MempoolRequest::GetState);
                                    pending_mempool_fetches.insert(request_id, respond_to);
                                    debug!("Sent MempoolRequest::GetState to peer {peer_id}");
                                }
                                None => {
                                    let _ = respond_to.send(Err(StryiNetworkError::other(
                                        "No connected peers available for mempool sync".to_string(),
                                    )));
                                }
                            }
                        }

                        Some(NetworkCommand::RefreshPeerServices { peer, respond_to }) => {
                            debug!("NetworkManager: Refreshing services for peer {}", peer);
                            let is_connected = self.connected_peers.read().await.contains_key(&peer);
                            if is_connected {
                                let _req_id = swarm
                                    .behaviour_mut()
                                    .services_info
                                    .send_request(&peer, ServicesInfoRequest::ListServices);
                                debug!(
                                    "Sent ListServices refresh to peer {} (req_id={:?})",
                                    peer, _req_id
                                );
                            } else {
                                debug!("Peer {} not connected, skipping service refresh", peer);
                            }
                            let _ = respond_to.send(());
                        }

                        // if the channel close we exit the run loop
                        None => {
                            info!("Command channel closed, exiting run loop.");
                            break;
                        }

                        }
                    }

                // --- libp2p events ---
                event = swarm.select_next_some() => {
                    self.process_event(&mut swarm, event, &mut pending_mempool_fetches).await;
                }

            }
        }

        info!("StryiNetworkManager run loop terminated gracefully.");
    }
}

impl StryiNetworkManager {
    fn push_services_to_peers(
        behaviour: &mut StryiBehaviour,
        peers: Vec<PeerId>,
        services: Vec<SignedServiceRecord>,
    ) -> usize {
        peers
            .into_iter()
            .map(|peer| {
                behaviour.services_info.send_request(
                    &peer,
                    ServicesInfoRequest::PushServices {
                        services: services.clone(),
                    },
                );
            })
            .count()
    }
}
