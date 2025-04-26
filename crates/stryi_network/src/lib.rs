#![allow(async_fn_in_trait)]
mod behaviour;
mod error;
mod manager;
mod model;
mod mempool;
mod services;

pub use behaviour::*;
pub use error::StryiNetworkError;
pub use manager::*;
pub use services::ServiceInfo;
use libp2p::{Multiaddr};

pub use libp2p::identity::{Keypair, ed25519, SigningError, DecodingError};
use stryi_core::transactions::Transaction;
use crate::model::BroadcastBlock;

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



impl StryiNetworkManager {

    //--- Some high-level methods ---  
    // TODO: Actually revise and define high-level methods for StryiNetworkManager for broadcasting blocks, transactions, etc

    // /// Sends provided mined block across the network to other nodes, may return error if sending message to NetworkManager fails
    // pub async fn publish_mined_block(&self, block: &Block) -> Result<(), StryiNetworkError> {
    //     let block_bytes = bincode::serde::encode_to_vec(block, standard()).expect("Serialization commonly does not falls");
    // 
    //     self.command_tx.send(NetworkCommand::PublishBlock(block_bytes)).await.map_err(StryiNetworkError::ChannelError)
    // }
    // 
    // 
    // /// Publishes provided transaction to other nodes via gossipsub, may return error if sending message to NetworkManager fails
    // pub async fn publish_transaction(&self, tx : &Transaction) -> Result<(), StryiNetworkError> {
    //     let transaction_bytes = bincode::serde::encode_to_vec(tx, standard()).expect("Serialization commonly does not falls");
    // 
    //     self.command_tx.send(NetworkCommand::PublishTransaction(transaction_bytes)).await.map_err(StryiNetworkError::ChannelError)
    // }
    // 
    // /// Gets current mempool state from random connected node for further synchronization
    //  async fn get_mempool_state(&self) -> Result<stryi_core::mempool::MemPoolSyncData, StryiNetworkError> {
    //     todo!()
    // }
}

