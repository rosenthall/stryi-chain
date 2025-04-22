use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};
use std::sync::Arc;
use bincode::config::standard;
use bincode::serde::decode_from_slice;
use futures::StreamExt;
use libp2p::{core::upgrade, identity::Keypair, noise, tcp, yamux, swarm::{Swarm, Config as SwarmConfig}, Transport, PeerId, Multiaddr, StreamProtocol, request_response, gossipsub};
use libp2p::gossipsub::{IdentTopic, MessageId};
use libp2p::request_response::{InboundRequestId, ProtocolSupport, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace};
use stryi_core::mempool::{MemPool};
use stryi_core::transactions::Transaction;
use crate::{RendezvousMode, error::StryiNetworkError, behaviour::{StryiBehaviour, StryiBehaviourConfig, StryiEvent}, StryiNetworkManagerConfig, NetworkCommand, NetworkEvent, behaviour};
use crate::mempool::{MempoolMessage, MempoolRequest, MempoolResponse, MempoolSyncBehaviour};
use crate::model::BroadcastBlock;
use crate::StryiNetworkError::CannotRespond;

/// StryiNetworkManager sets up the transport, constructs a swarm using our unified StryiBehaviour,
/// and runs the event loop.
/// Provides high-level communication layer with network via channels and messaging such as
/// - `command_tx` : Channel for communicating with entire network, allows performing operations like publish blocks/transactions, dial with specific node, etc.
/// - `event_tx` : Channel for
pub struct StryiNetworkManager {

    /// Configuration for entire StryiNetworkManager instance, defines addresses, keypair, rendezvous mode, etc.
    pub(crate) config: StryiNetworkManagerConfig,
    /// Swarm with specified behaviour (using StryiBehaviour)
    pub(crate) swarm: Arc<Mutex<Swarm<StryiBehaviour>>>,

    /// Arc reference to mempool object
    pub(crate) mempool: Arc<RwLock<MemPool>>,

    /// Channel to perform operation in network like sending blocks, transactions, dialing a connections, etc.
    pub command_tx: mpsc::Sender<NetworkCommand>,

    /// Receiver side for `command_tx`, must be used in the run loop.
    pub(crate) command_rx: mpsc::Receiver<NetworkCommand>,

    // Event channel: network manager broadcasts events (e.g., peer events) to subscribers
    pub event_tx: broadcast::Sender<NetworkEvent>,


    /// Connected peers tracking TODO : Actually track peers
    pub(crate) connected_peers: Arc<RwLock<HashMap<PeerId, PeerInfo>>>,


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
}


// hard‑coded topic names that every node must agree on
const BLOCKS_TOPIC_NAME: &str = "stryichain-blocks";
const TRANSACTIONS_TOPIC_NAME: &str = "stryichain-txs";

impl StryiNetworkManager {
    /// Creates a new StryiNetworkManager based on the provided configuration, Arc-ed mempool, and cancellation_token
    pub fn new(config: &StryiNetworkManagerConfig, mempool: Arc<RwLock<MemPool>>, cancel_token: CancellationToken) -> Result<Self, StryiNetworkError> {
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
        let listen_addr: Multiaddr = config
            .listen_addr
            .parse()
            .map_err(|e| StryiNetworkError::other(format!("Failed to parse listen address: {e}")))?;
        
        swarm
            .listen_on(listen_addr)
            .map_err(|e| StryiNetworkError::other(format!("Swarm listen error: {e:?}")))?;

        // If in Client mode and rendezvous-server address is provided, dial it
        if let RendezvousMode::Client = config.rendezvous_mode {
            if let Some(ref srv_addr) = config.rendezvous_server_addr {
                let ma: Multiaddr = srv_addr
                    .parse()
                    .map_err(|e| StryiNetworkError::other(format!("Failed to parse rendezvous server address: {e}")))?;
                swarm
                    .dial(ma)
                    .map_err(|e| StryiNetworkError::other(format!("Swarm dial error: {e:?}")))?;
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
            connected_peers: Arc::new(Default::default()),
            cancel_token,

        })
    }

    /// Returns a new receiver that can be used by callers to listen for network events.
    pub fn subscribe_events(&self) -> broadcast::Receiver<NetworkEvent> {
        self.event_tx.subscribe()
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

    pub async fn run_loop(&mut self) {
        let mut swarm = self.swarm.lock().await;
        
        // Subscribe on gossipsub topics for blocks and transactions
        let transactions_topic = IdentTopic::new(TRANSACTIONS_TOPIC_NAME);
        let blocks_topic = IdentTopic::new(BLOCKS_TOPIC_NAME);
        
        // TODO: Handle somehow gossipsub subscription error
        swarm.behaviour_mut().gossipsub
            .subscribe(&transactions_topic).unwrap();
        swarm.behaviour_mut().gossipsub
            .subscribe(&blocks_topic).unwrap();



        loop {
            tokio::select! {

                // Cancellation token handling
                _ = self.cancel_token.cancelled() => {
                    info!("Cancellation requested, exiting run_loop");
                    break;
                }

                command = self.command_rx.recv() => {
                    // TODO: Implement NetworkCommands handler in network-manager loop
                    info!("Received command : {command:?}");
                }
                
                // --- libp2p events ---
                event = swarm.select_next_some() =>
                    // match all the received events
                    match event {


                        // --- Utils ---
                        SwarmEvent::NewListenAddr { address, .. } => {
                            info!("Listening on {}", address);
                        }
                        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                            info!("Connected to {}", peer_id);
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            info!("Disconnected from {}", peer_id);
                        }

                        // -- Stryichain's custom behaviour events --
                        SwarmEvent::Behaviour(behaviour_event) => {
                            match behaviour_event {


                                // --- Mempool ---
                                StryiEvent::MempoolRequest(mempool_msg) => {
                                    if let MempoolMessage::Request  {request_id,request,channel} = mempool_msg  {
                                        // process respond and ignore possible errors
                                        self.handle_mempool_request(request_id, request, channel).await.ok();
                                    }
                                },

                                // --- Gossipsub ---
                                StryiEvent::Gossipsub(gossipsub_event) => {
                                    self.handle_gossipsub_event(gossipsub_event).await.ok();
                                },

                                // --- Identify ---
                                // TODO: Setup Identify events handling

                                // -- Get-services ---
                                // TODO: Setup ServicesInfo request-response service for a convenient way to get grpc server address


                                // -- temporal stubs --
                                StryiEvent::Identify(e)  => debug!("Identify event: {:?}", e),
                                StryiEvent::RzvServer(e) => debug!("Rendezvous Server event: {:?}", e),
                                StryiEvent::RzvClient(e) => debug!("Rendezvous Client event: {:?}", e),

                                // Fallback
                                other => {
                                    debug!("Got unhandled custom event: {:?}", other);
                                    }
                                }
                        }


                        // todo: process other events, such as dialing, gossipsub, rendezvous(server), identify, and ping. For now just log and keep looping
                        _ => debug!("Got event: {:?}", event),
                    }
                }
        }
        info!("run_loop terminated gracefully.");
    }


    // handles gossipsub requests
    async fn handle_gossipsub_event(&self, event : gossipsub::Event) -> Result<(), StryiNetworkError> {
        trace!("Got gossipsub event {:?}", event);

        match event {
            gossipsub::Event::Message {propagation_source, message_id, message } => {
                trace!("Got gossipsub message! Propagation source : {}, id : {}", propagation_source, &message_id.to_string());



                // Check the topics name and define how to proceed message correspondingly
                match message.topic.as_str() {

                    // Try to process everything from transactions topic as a transaction
                    TRANSACTIONS_TOPIC_NAME => {
                        let tx : Transaction = decode_from_slice(&*message.data, standard()).map_err(StryiNetworkError::DecodeGossipsubMessageError)?.0;
                        debug!("Received transaction {} in gossipsub", &tx.data.hash());

                        // try to generate and send `NewTransaction` event
                        self.event_tx.send(NetworkEvent::NewTransaction(tx)).map_err(StryiNetworkError::CannotSendEvent)?;
                    }


                     // and from blocks topic as a block
                     BLOCKS_TOPIC_NAME => {
                         let broadcast_block : BroadcastBlock = decode_from_slice(&*message.data, standard()).map_err(StryiNetworkError::DecodeGossipsubMessageError)?.0;
                         debug!("Received block {} in gossipsub", &broadcast_block.block.block_hash());

                         // Try generate and send `NewBlock` event
                         self.event_tx.send(NetworkEvent::NewBlock(broadcast_block)).map_err(StryiNetworkError::CannotSendEvent)?;

                     }

                    // We don't care about all another topics
                    _ => {}
                 };


                Ok(())
            },
            gossipsub::Event::Subscribed { .. } => Ok(()),
            gossipsub::Event::Unsubscribed { .. } => Ok(()),
            
            // We don't care about these two I guess
            gossipsub::Event::GossipsubNotSupported { .. } => Ok(()),
            gossipsub::Event::SlowPeer { .. } => Ok(()),
        }
    }

    // handles mempool requests
    async fn handle_mempool_request(&self, request_id: InboundRequestId, request : MempoolRequest , response_channel: ResponseChannel<MempoolResponse>) -> Result<(), StryiNetworkError> {
        trace!("Got mempool request {:?}, id : {}", &request, &request_id.to_string());


        // Construct MempoolSyncBehaviour instance
        let mut behaviour = MempoolSyncBehaviour::new(
            [(StreamProtocol::new("/mempool"), ProtocolSupport::Full)], // `protocols`
            request_response::Config::default() // `cfg`
        );

        let response = match request {
            MempoolRequest::GetState => {
                // construct MemPoolSyncData, log and return error if any
                let state = self.mempool.read().await
                    .get_sync_state().await
                    .map_err(|e| {
                        error!("Got error from tx mempool while trying to get MemPoolSyncData: {e}");
                        StryiNetworkError::MempoolError(e)
                    })?;

                // Wrap it in MempoolResponse
                MempoolResponse::State(state)
            },
            
            // something more I'll need?
        };
        
        // try respond
        let res = behaviour.send_response(response_channel, response);
        if res.is_err() {
            error!("Cannot send Response for mempool request, request id : {}", &request_id.to_string());
            return Err(CannotRespond(request_id));
        }
        
        Ok(())
    }
}