#![allow(async_fn_in_trait)]
#![allow(clippy::result_large_err)]

mod behaviour;
mod error;
mod event;
mod manager;
mod mempool;
mod model;
mod peer;
mod services;

pub use crate::model::{BroadcastBlock, ChainTipAnnouncement};
pub use behaviour::*;
pub use error::StryiNetworkError;
pub use libp2p::{Multiaddr, PeerId};
pub use manager::*;

pub use crate::services::{ServiceRecord, ServiceTransportSecurity};
use libp2p::identity::PublicKey;
pub use libp2p::identity::{DecodingError, Keypair, SigningError, ed25519};
use stryi_core::mempool::MemPoolSyncData;
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
/// Every variant carries a `respond_to` oneshot so the caller can observe the outcome.
#[derive(Debug)]
pub enum NetworkCommand {
    /// Publish a new block to the network via gossipsub.
    PublishBlock {
        block: BroadcastBlock,
        respond_to: tokio::sync::oneshot::Sender<Result<(), StryiNetworkError>>,
    },

    /// Publish a new transaction to the network via gossipsub.
    PublishTransaction {
        transaction: Transaction,
        respond_to: tokio::sync::oneshot::Sender<Result<(), StryiNetworkError>>,
    },

    /// Get the current network status, including connected peers and their addresses.
    /// This returns only service-records that were signed and already validated.
    QueryPeersWithService {
        service: String,
        respond_to: tokio::sync::oneshot::Sender<Vec<(PeerId, ServiceRecord)>>,
    },

    /// Get the public key of a peer by its PeerId.
    QueryPeerPublicKey {
        peer: PeerId,
        respond_to: tokio::sync::oneshot::Sender<Option<PublicKey>>,
    },

    /// Signs with an own private key and adds service record to advertise registry
    /// After that, any peer can query this service record and use it.
    AddService {
        service: ServiceRecord,
        respond_to: tokio::sync::oneshot::Sender<Result<(), StryiNetworkError>>,
    },

    /// Requests mempool state from a random peer.
    /// Network manager returns `MemPoolSyncData` via oneshot.
    /// note: *The applying this state on local impl is caller's duty*
    FetchMempoolState {
        respond_to: tokio::sync::oneshot::Sender<Result<MemPoolSyncData, StryiNetworkError>>,
    },

    /// Publish a chain tip announcement to the network via gossipsub.
    PublishChainTip {
        announcement: ChainTipAnnouncement,
        respond_to: tokio::sync::oneshot::Sender<Result<(), StryiNetworkError>>,
    },

    /// Re-query a specific peer's services via the existing ListServices protocol.
    /// The response is cached automatically by the existing response handler.
    /// The caller should wait a short time after this returns for the cache to update.
    RefreshPeerServices {
        peer: PeerId,
        respond_to: tokio::sync::oneshot::Sender<()>,
    },
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
    /// A chain tip announcement was received from a peer.
    ChainTipAnnounced {
        announcement: ChainTipAnnouncement,
        source: PeerId,
    },
}
