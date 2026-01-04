use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::Duration,
};

use crate::error::{StryiNetworkError, StryiNetworkError::GossipsubConfigError};
use crate::mempool::{MempoolEvent, MempoolSyncBehaviour};
use crate::services::{ServicesEvent, ServicesInfoBehaviour};
use libp2p::identity::Keypair;
use libp2p::request_response::ProtocolSupport;
use libp2p::{
    StreamProtocol,
    gossipsub::{
        Behaviour as Gossipsub, ConfigBuilder as GossipsubConfigBuilder, Event as GossipsubEvent,
        MessageAuthenticity, MessageId, ValidationMode,
    },
    identify::{Behaviour as Identify, Config as IdentifyConfig, Event as IdentifyEvent},
    ping::{Behaviour as Ping, Config as PingConfig, Event as PingEvent},
    rendezvous::{
        client::{Behaviour as RzvClient, Event as RzvClientEvent},
        server::{Behaviour as RzvServer, Config as RzvServerConfig, Event as RzvServerEvent},
    },
    swarm::{NetworkBehaviour, behaviour::toggle::Toggle},
};

/// High-level event combining all sub-protocol events.
#[derive(Debug)]
pub enum StryiEvent {
    Gossipsub(GossipsubEvent),
    Ping(PingEvent),
    Identify(IdentifyEvent),
    RzvServer(RzvServerEvent),
    RzvClient(RzvClientEvent),
    Mempool(MempoolEvent),
    Services(ServicesEvent),
}

impl From<GossipsubEvent> for StryiEvent {
    fn from(e: GossipsubEvent) -> Self {
        StryiEvent::Gossipsub(e)
    }
}
impl From<PingEvent> for StryiEvent {
    fn from(e: PingEvent) -> Self {
        StryiEvent::Ping(e)
    }
}
impl From<IdentifyEvent> for StryiEvent {
    fn from(e: IdentifyEvent) -> Self {
        StryiEvent::Identify(e)
    }
}
impl From<RzvServerEvent> for StryiEvent {
    fn from(e: RzvServerEvent) -> Self {
        StryiEvent::RzvServer(e)
    }
}
impl From<RzvClientEvent> for StryiEvent {
    fn from(e: RzvClientEvent) -> Self {
        StryiEvent::RzvClient(e)
    }
}

/// Configuration for building a `StryiBehaviour`.
#[derive(Debug, Clone)]
pub struct StryiBehaviourConfig {
    /// If true, enable the Rendezvous server sub-behavior.
    pub enable_rendezvous_server: bool,

    /// If true, enable the Rendezvous client sub-behavior.
    pub enable_rendezvous_client: bool,

    /// Ping interval.
    pub ping_interval: Duration,

    /// Ping timeout.
    pub ping_timeout: Duration,

    /// Gossipsub heartbeat interval.
    pub gossipsub_heartbeat: Duration,
}

impl Default for StryiBehaviourConfig {
    fn default() -> Self {
        Self {
            enable_rendezvous_server: true, // By default, the node acts as if the network is newly established and assumes the role of a rendezvous server
            enable_rendezvous_client: false, // The node does not act as a rendezvous client by default
            ping_interval: Duration::from_secs(10),
            ping_timeout: Duration::from_secs(10),
            gossipsub_heartbeat: Duration::from_secs(10),
        }
    }
}

/// A single `NetworkBehaviour` that includes Gossipsub, Ping, Identify, ServicesInfo
/// and a Toggle-wrapped (optional) Rendezvous server or client
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "StryiEvent")]
pub struct StryiBehaviour {
    pub gossipsub: Gossipsub,
    #[behaviour(ignore_events)]
    // todo: Do we need more complex logic for pinging? Some analytics for RTT, latency, etc
    pub ping: Ping,
    pub identify: Identify,
    pub rendezvous_server: Toggle<RzvServer>,
    pub rendezvous_client: Toggle<RzvClient>,
    pub mempool_sync: MempoolSyncBehaviour,
    pub services_info: ServicesInfoBehaviour,
}

impl StryiBehaviour {
    /// Create a new `StryiBehaviour` from the given configuration.
    /// Returns an error if building the gossipsub configuration fails.
    pub fn new(
        cfg: StryiBehaviourConfig,
        keypair: &Keypair,
        _protocol_version: &usize,
    ) -> Result<Self, StryiNetworkError> {
        // Build configured Gossipsub
        let gossipsub = Self::build_gossipsub(&cfg, keypair)?;

        // Build Ping with the specified interval and timeout.
        let ping_cfg = PingConfig::new()
            .with_interval(cfg.ping_interval)
            .with_timeout(cfg.ping_timeout);
        let ping = Ping::new(ping_cfg);

        // --- request-response behaviours ---
        let mempool_sync = MempoolSyncBehaviour::new(
            [(
                StreamProtocol::new("/stryichain/mempool"),
                ProtocolSupport::Full,
            )],
            libp2p::request_response::Config::default(),
        );

        let services_info = ServicesInfoBehaviour::new(
            [(
                StreamProtocol::new("/stryichain/services"),
                ProtocolSupport::Full,
            )],
            libp2p::request_response::Config::default(),
        );

        // Build Identify with a fixed protocol version.
        let identify = Self::build_identify(keypair);

        // Set up Rendezvous toggles.
        let rendezvous_server = if cfg.enable_rendezvous_server {
            Toggle::from(Some(RzvServer::new(RzvServerConfig::default())))
        } else {
            Toggle::from(None)
        };
        let rendezvous_client = if cfg.enable_rendezvous_client {
            Toggle::from(Some(RzvClient::new(keypair.clone())))
        } else {
            Toggle::from(None)
        };

        Ok(Self {
            gossipsub,
            ping,
            identify,
            mempool_sync,
            services_info,
            rendezvous_server,
            rendezvous_client,
        })
    }

    /// Helper to build identify behaviour with automatic listen address updates.
    pub fn build_identify(keypair: &Keypair) -> Identify {
        let cfg = IdentifyConfig::new("stryichain/0.1.0".to_string(), keypair.public())
            .with_push_listen_addr_updates(true); // Enable automatic updates of listen addresses
        Identify::new(cfg)
    }

    /// Helper to build a custom gossipsub behaviour instance using the provided `StryiBehaviourConfig` and a `keypair`.
    fn build_gossipsub(
        cfg: &StryiBehaviourConfig,
        keypair: &Keypair,
    ) -> Result<Gossipsub, StryiNetworkError> {
        let msg_id_fn = |msg: &libp2p::gossipsub::Message| {
            let mut hasher = DefaultHasher::new();
            msg.data.hash(&mut hasher);
            MessageId::from(hasher.finish().to_string())
        };

        let gossipsub_config = GossipsubConfigBuilder::default()
            .heartbeat_interval(cfg.gossipsub_heartbeat)
            .validation_mode(ValidationMode::Strict)
            .message_id_fn(msg_id_fn)
            .build()
            .map_err(GossipsubConfigError)?;

        let gossipsub_behaviour = Gossipsub::new(
            MessageAuthenticity::Signed(keypair.clone()),
            gossipsub_config,
        )
        .unwrap();

        Ok(gossipsub_behaviour)
    }
}
