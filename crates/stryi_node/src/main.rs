#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199


mod node;
mod grpc;
mod error;
mod mining_manager;
mod keys;

/// Helper functions for generating x.509 certificates for node's services
mod tls;

use std::error::Error;
use std::io::{ErrorKind, Read};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use clap::Parser;
use std::time::Duration;
use colored::Colorize;
use tokio::io;
use tokio::time::sleep;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use stryi_core::mempool::{FeePolicy, MemPool, MemPoolConfig, RbfPolicy, UtxoLookup};
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::OutPoint;
use stryi_network::{StryiBehaviourConfig, StryiNetworkManager, StryiNetworkManagerConfig, RendezvousMode, ServiceInfo};
use stryi_storage::{GenesisInitConfig, StryiStorage};
use crate::grpc::{StryiSyncServiceConfig};
use crate::keys::PeerKey;
use crate::node::StryiChainNode;
use crate::tls::cert_and_key_from_peer;

pub(crate) mod grpc_services {
    tonic::include_proto!("stryi.sync");
}

/// Command-line arguments for the Stryi network node.
#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    /// Run in server mode (if not set, runs in node mode)
    #[arg(long)]
    rendezvous: bool,

    /// Multiaddr to listen on (e.g., "/ip4/0.0.0.0/tcp/1234")
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/1234")]
    listen_addr: String,

    /// Path to the database
    #[arg(long, default_value = "/var/lib/stryi_chain")]
    database_dir_path : String,
    
    /// Path to .json file with wanted configuration for genesis block
    #[arg(long, required = false)]
    genesis_config_path: Option<String>,

    /// Path to peer-key backup file
    #[arg(long, default_value = "/var/lib/stryi_chain/peer.stryi_keys")]
    peer_key_path: String,
    
    /// Rendezvous server multiaddr (used in node mode only)
    #[arg(long)]
    rendezvous_address: Option<String>,

    /// Rendezvous namespace
    #[arg(long, default_value = "stryi-rendezvous")]
    rendezvous_namespace: String,
}

/// Reads and deserializes the config from provided path.
fn try_genesis_config_from_path(path : PathBuf) ->  Result<GenesisInitConfig, Box<dyn Error>> {
    
    // Check if file exists and if it is a file.
    // .exists() method is redundant since is_file() already checks it
    if !path.is_file() { 
        return Err(Box::new(io::Error::new(ErrorKind::NotFound, "Provided path with genesis configuration is not a file or doesn't exists.")))
    }
    
    let mut file = std::fs::File::open(&path)?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    
    
    // Try to deserialize 
   serde_json::from_str(&buf).map_err(|e| Box::new(e) as Box<dyn Error>)
    
}


fn print_essentials() {
    println!("{}", "Welcome to the StryiChain Node CLI !".bright_yellow());
    println!("- Node version: {}", env!("CARGO_PKG_VERSION").green());
    println!("- Description: {}", env!("CARGO_PKG_DESCRIPTION").white());
    println!("My {}: https://github.com/rosenthall", "GitHub".green());
    println!("StryiChain {} repository: https://github.com/rosenthall/stryi-chain/", "GitHub".green());

    println!("{}{}",
    r#"
    █▀▀ ▀█▀ █▀█ ▀▄▀ ▀█▀  █▀▀ █▄█ ▄▀▄ ▀█▀ █▄ █
    ▄██  █  █▀▄  █  ▄█▄  █▄▄ █ █ █▀█ ▄█▄ █ ▀█
    "#.blue(),
    r#"
                █▄ █ █▀█ █▀▄ █▀▀
                █ ▀█ █▄█ █▄▀ ██▄
    "#.green().on_black())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {

    print_essentials();

    // Initialize the tracing subscriber. TODO: Make logging better, filter useless stuff like h2, handshakes, etc.. `env-filter` feature for tracing-subscriber would be helpful
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish();
    
    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    // Parse command-line arguments.
    let args = Args::parse();
    

    // TODO: make configuration of sync service actually configurable from CLI
    let sync_service_config = StryiSyncServiceConfig {
        address: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(0,0,0,0), 5555)),
        chain_name : "dev".to_string(),
        protocol_version : 1,
        max_blocks_range_per_request: 100,
    };

    let genesis_config = if args.genesis_config_path.is_some() {
        info!("Genesis config path is provided, trying to deserialize config.");
        Some(try_genesis_config_from_path(PathBuf::from(args.genesis_config_path.unwrap()))?)
    } else {
        None
    };
    
    // Initializing storage in provided path
    let storage = StryiStorage::initialize_in_path(PathBuf::from(args.database_dir_path), genesis_config).await?;
    let storage = Arc::new(RwLock::new(storage));



    // TODO: Make mempool configurable as well
    let mempool_config = MemPoolConfig::new(100, FeePolicy::default(), RbfPolicy::default(), 36000);

    // Create utxo_lookup for mempool that reads UTXO by outpoint from storage
    let utxo_lookup: UtxoLookup = {
        let storage = storage.clone();

        // Closure captures Arc-ed storage
        Box::new(move |out_point: &OutPoint| {
            // Clone storage for the async block
            let storage = storage.clone();

            // Copy outpoint by value into async block
            let out_point = *out_point;

            // Return boxed async future that reads UTXO
            Box::pin(async move {
                storage.read().await.get_utxo(&out_point).await.ok()
            })
        })
    };



    let mempool = Arc::new(RwLock::new(MemPool::new(mempool_config, utxo_lookup)));


    // -- Initialize NetworkManager --
    
    
    // Backup peer key
    let peer_key = match PeerKey::restore(&args.peer_key_path) {
        Ok(k) => {
            info!("Restored peer key from {}", &args.peer_key_path);
            k
        }
        Err(_) => {
            info!("No existing peer key, generating a fresh one");
            let fresh = PeerKey::generate_random();
            // Ignore I/O error on first run; report only if backup fails later.
            let _ = fresh.backup(&args.peer_key_path);
            fresh
        }
    };

    let keypair = peer_key.inner().clone(); // clone to hand over to NetworkManager



    // generate tls identity for services of node
    let tls_identity = cert_and_key_from_peer(&keypair, &["localhost"])  // TODO: setup SANs somehow better
        .expect("Cannot generate certificate based on this peer's keypair");

    info!("Generated certificate for node services! This node certificate :");
    println!("{}", tls_identity.cert_pem.as_str().purple());


    let behaviour_config = StryiBehaviourConfig::default();
    let network_manager_config = StryiNetworkManagerConfig {
        rendezvous_mode: RendezvousMode::Server,
        keypair: keypair.clone(),
        stryi_behaviour_config: behaviour_config,
        ..Default::default()
    };

    let network_manager_cancellation_token = CancellationToken::new();

    // An initially empty list – we'll fill it later when services start.
    let services_info: Arc<RwLock<Vec<ServiceInfo>>> = Arc::new(RwLock::new(Vec::new()));
    
    let network_manager = StryiNetworkManager::new(&network_manager_config, mempool.clone(), services_info.clone(), network_manager_cancellation_token)?;

    // Instantiate the StryiChainNode
    let node = StryiChainNode {
        mempool,
        storage,
        network_manager,
        services_info,
        tls_identity,
        sync_service_config,
        grpc_is_ready: Arc::new(RwLock::new(false)),
    };

    node.start_services().await?;
    
    // Keep the node running indefinitely.
    loop {
        sleep(Duration::from_secs(60)).await;
    }
}
