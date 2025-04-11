#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199


mod node;
mod grpc;
mod error;
mod mining_manager;

use std::error::Error;
use std::io::{ErrorKind, Read};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use clap::Parser;
use std::time::Duration;
use colored::Colorize;
use tokio::io;
use tokio::time::sleep;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use tokio::sync::RwLock;
use stryi_network::{StryiBehaviourConfig, StryiNetworkManager, StryiNetworkManagerState, StryiNodeMode};
use stryi_storage::{GenesisInitConfig, StryiStorage};
use crate::grpc::{StryiSyncServiceConfig};
use crate::node::StryiChainNode;

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
    "#.yellow())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {

    print_essentials();

    // Initialize the tracing subscriber.
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

    // Initalize NetworkManager

    let keypair =  stryi_network::Keypair::generate_ed25519(); // TODO: make node's keypair configurable.

    let behaviour_config = StryiBehaviourConfig {
        keypair : keypair.clone(),
        enable_server: true,
        ..Default::default()
    };

    let network_manager_state = StryiNetworkManagerState {
        mode: StryiNodeMode::Server,
        keypair: Some(keypair.clone()),
        stryi_behaviour_config: behaviour_config,
        ..Default::default()

    };
    
    
    let network_manager = StryiNetworkManager::new(&network_manager_state)?;

    // Instantiate the StryiChainNode
    let node = StryiChainNode {
        storage,
        network_manager,
        sync_service_config,
    };
    
    node.start_services().await?;
    
    // Keep the node running indefinitely.
    loop {
        sleep(Duration::from_secs(60)).await;
    }
}
