use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::Duration,
};

use libp2p::{
    // Gossipsub
    gossipsub::{
        Behaviour as Gossipsub, Event as GossipsubEvent, MessageAuthenticity,
        MessageId, ValidationMode, Config as GossipsubConfig,
        ConfigBuilder as GossipsubConfigBuilder,
    },
    // Identify
    identify::{Behaviour as Identify, Event as IdentifyEvent, Config as IdentifyConfig},
    // Ping
    ping::{Behaviour as Ping, Event as PingEvent, Config as PingConfig},
    // Rendezvous: both server and client.
    rendezvous::{
        server::{Behaviour as RzvServer, Event as RzvServerEvent, Config as RzvServerConfig},
        client::{Behaviour as RzvClient, Event as RzvClientEvent},
    },
    // Import the trait and derive macro (the derive macro is re-exported by libp2p::swarm)
    swarm::{
        NetworkBehaviour,
        behaviour::toggle::Toggle,
    },
    identity::Keypair,
};

use crate::error::{StryiNetworkError, StryiNetworkError::GossipsubConfigError};

/// High-level event combining all sub-protocol events.
#[derive(Debug)]
pub enum StryiEvent {
    Gossipsub(GossipsubEvent),
    Ping(PingEvent),
    Identify(IdentifyEvent),
    RzvServer(RzvServerEvent),
    RzvClient(RzvClientEvent),
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
    /// The keypair for signing messages and for identity.
    pub keypair: Keypair,
    /// If true, enable the Rendezvous server sub-behavior.
    pub enable_server: bool,
    /// If true, enable the Rendezvous client sub-behavior.
    pub enable_client: bool,
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
            keypair: Keypair::generate_ed25519(),
            enable_server: false,
            enable_client: false,
            ping_interval: Duration::from_secs(10),
            ping_timeout: Duration::from_secs(10),
            gossipsub_heartbeat: Duration::from_secs(10),
        }
    }
}

/// A single `NetworkBehaviour` that includes Gossipsub, Ping, Identify,
/// plus optional Rendezvous server and client (via Toggle).
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "StryiEvent")]
pub struct StryiBehaviour {
    pub gossipsub: Gossipsub,
    pub ping: Ping,
    pub identify: Identify,
    pub rendezvous_server: Toggle<RzvServer>,
    pub rendezvous_client: Toggle<RzvClient>,
}

impl StryiBehaviour {
    /// Create a new `StryiBehaviour` from the given configuration.
    /// Returns an error if building the gossipsub configuration fails.
    pub fn new(cfg: StryiBehaviourConfig) -> Result<Self, StryiNetworkError> {
        // Build Gossipsub with a custom message ID function.
        let gossipsub = Self::build_gossipsub(&cfg)?;

        // Build Ping with the specified interval and timeout.
        let ping_cfg = PingConfig::new()
            .with_interval(cfg.ping_interval)
            .with_timeout(cfg.ping_timeout);
        let ping = Ping::new(ping_cfg);

        // Build Identify with a fixed protocol version.
        let id_cfg = IdentifyConfig::new("stryichain/1.0.0".to_string(), cfg.keypair.public());
        let identify = Identify::new(id_cfg);

        // Set up Rendezvous toggles.
        let r_server = if cfg.enable_server {
            Toggle::from(Some(RzvServer::new(RzvServerConfig::default())))
        } else {
            Toggle::from(None)
        };

        let r_client = if cfg.enable_client {
            Toggle::from(Some(RzvClient::new(cfg.keypair.clone())))
        } else {
            Toggle::from(None)
        };

        Ok(Self {
            gossipsub,
            ping,
            identify,
            rendezvous_server: r_server,
            rendezvous_client: r_client,
        })
    }

    /// Helper to build a custom gossipsub instance using the provided configuration.
    fn build_gossipsub(cfg: &StryiBehaviourConfig) -> Result<Gossipsub, StryiNetworkError> {
        let msg_id_fn = |msg: &libp2p::gossipsub::Message| {
            let mut hasher = DefaultHasher::new();
            msg.data.hash(&mut hasher);
            MessageId::from(hasher.finish().to_string())
        };

        let gconfig = GossipsubConfigBuilder::default()
            .heartbeat_interval(cfg.gossipsub_heartbeat)
            .validation_mode(ValidationMode::Strict)
            .message_id_fn(msg_id_fn)
            .build()
            .map_err(|e| GossipsubConfigError(e))?;

        let gsub = Gossipsub::new(
            MessageAuthenticity::Signed(cfg.keypair.clone()),
            gconfig,
        ).unwrap();

        Ok(gsub)
    }
}
