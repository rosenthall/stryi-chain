#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199


mod node;
mod grpc;
mod error;

use std::error::Error;
use std::io::{ErrorKind, Read};
use std::path::PathBuf;
use std::sync::Arc;
use clap::Parser;
use std::time::Duration;
use tokio::io;
use tokio::time::sleep;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;
use tokio::sync::RwLock;
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


fn print_essential_info() {
    info!("Hello world");
    info!("- Version: {}", env!("CARGO_PKG_VERSION"));
    info!("- Description: {}", env!("CARGO_PKG_DESCRIPTION"));
    info!("GitHub: github.com/rosenthall");
    info!("Starting node");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    
    
    // TODO: Some thiserror enum for common errors for this target.
    
    // Initialize the tracing subscriber.
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    // Parse command-line arguments.
    let args = Args::parse();
    
    print_essential_info();

    // TODO: make configuration of sync service actually configurable from CLI
    let sync_service_config = StryiSyncServiceConfig {
        port : 222,
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

    
    // Instantiate the StryiChainNode   
    let node = StryiChainNode {
        storage,
        stryi_sync_service_config: sync_service_config,
    };
    
    node.start().await?;
    
    
    
    // Keep the node running indefinitely.
    loop {
        sleep(Duration::from_secs(60)).await;
    }
}
