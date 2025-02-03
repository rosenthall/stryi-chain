#![allow(async_fn_in_trait)]
mod behaviour;
mod node_config;
mod error;
mod manager;


use futures::Stream;
use std::error::Error;
use std::pin::Pin;
use std::time::Duration;
use libp2p::identity::Keypair;
use libp2p::{PeerId, Swarm};
use tokio::time::sleep;
use crate::behaviour::{StryiBehaviour, StryiBehaviourConfig};
use crate::error::StryiNetworkError;
use crate::manager::StryiNetworkManager;
use crate::node_config::StryiNodeMode;

/// Commands that can be sent to the network service.
#[derive(Debug, Clone)]
pub enum NetworkCommand {
    /// Dial a remote peer using the provided multiaddr.
    Dial { address: String },
    /// Publish a new block to the network.
    PublishBlock(Vec<u8>),
    /// Publish a new transaction to the network.
    PublishTransaction(Vec<u8>),
}

/// Events that are emitted by the network service.
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// A new block was received.
    NewBlock(Vec<u8>),
    /// A new transaction was received.
    NewTransaction(Vec<u8>),
    /// A new peer has connected.
    PeerConnected(String),
    /// A peer has disconnected.
    PeerDisconnected(String),
    
    
    
    
    // Something more I need?
}

/// Represents the current status of the network service.
#[derive(Debug, Clone)]
pub enum ServiceStatus {
    Running,
    Stopped,
    Restarting,
}

/// An asynchronous trait that provides a high-level abstraction over the network layer.
/// This trait is designed to be extended and decouples the low-level P2P implementation
/// from the blockchain application logic.
pub trait NetworkService {
    /// The type of error produced by the network service.
    type Error: Error + Send + Sync + 'static;
    /// The type of events produced by the network service.
    type Event: Send + 'static;
    /// The associated configuration type.
    type Config: Clone + Send + Sync + 'static;

    /// Starts the network service (for example, spawns the event loop).
    async fn start(&mut self) -> Result<(), Self::Error>;

    /// Returns an asynchronous stream of network events.
    async fn subscribe(&mut self) -> Pin<Box<dyn Stream<Item = Self::Event> + Send>>;

    /// Sends a generic command to the network service.
    async fn send_command(&mut self, cmd: NetworkCommand) -> Result<(), Self::Error>;

    /// Proposes a new block to be broadcast on the network.
    async fn propose_block(&mut self, block_data: Vec<u8>) -> Result<(), Self::Error>;

    /// Adds a new transaction and broadcasts it on the network.
    async fn add_transaction(&mut self, tx_data: Vec<u8>) -> Result<(), Self::Error>;

    /// Gracefully stops the network service.
    async fn stop(&mut self) -> Result<(), Self::Error>;

    /// Restarts the network service, optionally using a new configuration.
    async fn restart(&mut self, new_config: Option<Self::Config>) -> Result<(), Self::Error>;

    /// Returns the current status of the network service.
    async fn status(&self) -> ServiceStatus;
}





impl NetworkService for StryiNetworkManager {
    type Error = StryiNetworkError;
    type Event = NetworkEvent;
    type Config = crate::node_config::StryiNodeConfig;
    
    async fn start(&mut self) -> Result<(), Self::Error> {
        {
            let mut status = self.status.write().await;
            *status = ServiceStatus::Running;
        }
        // Spawn the run_loop with the cancellation token.
        let token = self.cancel_token.clone();
        // We take the swarm out via run_loop (which will put it back on exit).
        let status_clone = self.status.clone();
        let handle = tokio::spawn(async move {
            // NOTE: In this closure we cannot access self directly.
            // We assume that run_loop is already spawned via self.start().
            // In a real-world design, you might encapsulate the swarm in an Arc<Mutex<...>>.
            // For this example, we simply call run_loop.
            // (If you want to use the manager's run_loop, refactor accordingly.)
            // This dummy loop is for demonstration.
            token.cancelled().await;
            let mut status = status_clone.write().await;
            *status = ServiceStatus::Stopped;
        });
        self.handle = Some(handle);
        // Alternatively, you could call self.run_loop(token) directly here.
        Ok(())
    }
    
    async fn subscribe(&mut self) -> Pin<Box<dyn Stream<Item = Self::Event> + Send>> {
        // For demonstration, create an interval stream emitting a dummy event every 3 seconds.


        Box::pin(futures::stream::unfold((), |()| async {
            sleep(Duration::from_secs(3)).await;
            Some((NetworkEvent::NewBlock(vec![0; 32]), ()))
        }))
    }
    
    async fn send_command(&mut self, cmd: NetworkCommand) -> Result<(), Self::Error> {
        tracing::info!("Received command: {:?}", cmd);
        // TODO: Process commands (e.g., dial a peer, publish a message)
        Ok(())
    }
    
    async fn propose_block(&mut self, block_data: Vec<u8>) -> Result<(), Self::Error> {
        tracing::info!("Proposing block with {} bytes", block_data.len());
        // TODO: Publish block via gossipsub.
        Ok(())
    }
    
    async fn add_transaction(&mut self, tx_data: Vec<u8>) -> Result<(), Self::Error> {
        tracing::info!("Publishing transaction with {} bytes", tx_data.len());
        // TODO: Publish transaction via gossipsub.
        Ok(())
    }
    
    async fn stop(&mut self) -> Result<(), Self::Error> {
        tracing::info!("Stopping network service");
        self.cancel_token.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
        {
            let mut status = self.status.write().await;
            *status = ServiceStatus::Stopped;
        }
        Ok(())
    }
    
    async fn restart(&mut self, new_config: Option<Self::Config>) -> Result<(), Self::Error> {
        tracing::info!("Restarting network service");
        {
            let mut status = self.status.write().await;
            *status = ServiceStatus::Restarting;
        }
        self.stop().await?;
        if let Some(cfg) = new_config {
            self.config = cfg;
        }
        // Rebuild the swarm using the current configuration.
        let key = self
            .config
            .clone()
            .keypair
            .unwrap_or_else(|| Keypair::generate_ed25519());
        let local_peer_id = PeerId::from(key.public());
        let transport = Self::build_transport(&key)?;
        let behaviour_config = StryiBehaviourConfig {
            keypair: key.clone(),
            enable_server: matches!(self.config.mode, StryiNodeMode::Server),
            enable_client: matches!(self.config.mode, StryiNodeMode::Node),
            ping_interval: Duration::from_secs(10),
            ping_timeout: Duration::from_secs(10),
            gossipsub_heartbeat: Duration::from_secs(10),
        };
        let behaviour = StryiBehaviour::new(behaviour_config)?;
        let swarm_config = libp2p::swarm::Config::with_tokio_executor();
        self.swarm = Some(Swarm::new(transport, behaviour, local_peer_id, swarm_config));
        {
            let mut status = self.status.write().await;
            *status = ServiceStatus::Running;
        }
        self.start().await?;
        Ok(())
    }
    
    async fn status(&self) -> ServiceStatus {
        self.status.read().await.clone()
    }
}