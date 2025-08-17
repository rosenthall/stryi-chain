#![allow(async_fn_in_trait)]
mod behaviour;
mod error;
mod manager;
mod model;
mod mempool;
mod services;
mod event;

pub use behaviour::*;
pub use error::StryiNetworkError;
pub use manager::*;
pub use services::ServiceInfo;
pub use crate::model::BroadcastBlock;
use libp2p::{Multiaddr};

pub use libp2p::identity::{Keypair, ed25519, SigningError, DecodingError};
use stryi_core::transactions::Transaction;

/// Indicates whether we run as a Rendezvous **Server** or a **Client** node.
#[derive(Debug, Clone)]
pub enum RendezvousMode {
    /// Rendezvous Server
    Server,
    /// Regular Rendezvous Client (connects to a server, discovers peers)
    Client,
}

/// Global config for a NetworkManager: addresses, keypair, mode, etc.
#[derive(Debug, Clone)]
pub struct StryiNetworkManagerConfig {
    /// Which mode to run rendezvous (Server or Node).
    pub rendezvous_mode: RendezvousMode,

    /// Multiaddr to listen on, e.g. `/ip4/0.0.0.0/tcp/62649`.
    /// If you specify `/tcp/0` it picks a random port.
    pub listen_addr: String,

    /// If we are in Node mode, we can optionally dial a Rendezvous server,
    /// e.g. `/ip4/127.0.0.1/tcp/62649/p2p/<PEER_ID>`
    pub rendezvous_server_addr: Option<String>,

    /// Rendezvous namespace, e.g. `"stryichain"`.
    pub rendezvous_namespace: String,

    /// Identity Ed25519 key of the node
    pub keypair: Keypair,
    
    /// Numeric protocol version
    pub version: usize,

    /// Config for the p2p behaviour.
    pub stryi_behaviour_config: StryiBehaviourConfig,
}

impl Default for StryiNetworkManagerConfig {
    /// **note** default value for keypair is random.
    fn default() -> Self {
        Self {
            rendezvous_mode: RendezvousMode::Client,
            listen_addr: "/ip4/127.0.0.1/tcp/0".to_string(),
            rendezvous_server_addr: None,
            rendezvous_namespace: "stryichain".to_string(),
            keypair: Keypair::generate_ed25519(),
            version: 1,
            stryi_behaviour_config: Default::default(),
        }
    }
}


/// Commands that can be sent to the network service.
#[derive(Debug, Clone)]
pub enum NetworkCommand {
    // /// Dial a remote peer using the provided multiaddr.
    // Dial { address: String },
    /// Publish a new block to the network.
    PublishBlock(BroadcastBlock),
    
    /// Publish a new transaction to the network.
    PublishTransaction(Transaction),
    /// Gets current mempool state from random connected node and synchronizes it with own.
    SyncMempoolState,
}

/// Events that are emitted by the network service.
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// A new mined block was received.
    NewBlock(BroadcastBlock),
    /// A new transaction was received.
    NewTransaction(Transaction),
    /// A new peer has connected.
    PeerConnected(Multiaddr),
    /// A peer has disconnected.
    PeerDisconnected(Multiaddr),
    
    // Something more I need?
}


