#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199


mod node;
mod grpc;
mod error;
mod mining_manager;
mod keys;

/// Helper functions for generating x.509 certificates for node's services
mod tls;

/// Runtime configuration object for the node.
mod config;

/// Command-line overrides for node configuration.
mod cli;


use std::error::Error;
use std::io::{ErrorKind, Read};
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use colored::Colorize;
use tokio::io;
use tokio::time::sleep;
use tracing::{error, info, Level};
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
use crate::config::NodeConfig;
use crate::error::StryiNodeError;

pub(crate) mod grpc_services {
    tonic::include_proto!("stryi.sync");
}


/// Reads and deserializes the config from provided path.
fn try_genesis_config_from_path(path : PathBuf) ->  Result<GenesisInitConfig, StryiNodeError> {
    // Check if file exists and if it is a file.
    // .exists() method is redundant since is_file() already checks it
    if !path.is_file() { 
        return Err(StryiNodeError::Io(io::Error::new(ErrorKind::NotFound, "Provided path with genesis configuration is not a file or doesn't exists.")));
    }
    
    let mut file = std::fs::File::open(&path)?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    
    // Try to deserialize 
    serde_json::from_str(&buf)
        .map_err(|e| StryiNodeError::other(format!("Cannot deserialize genesis configuration, error : {}", e.to_string())))
    
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
    "#.green().on_black());

    println!("{}", "Starting..".blink().green());
    thread::sleep(Duration::from_secs(3));
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {

    // Initialize the tracing subscriber. TODO: Make logging better, filter useless stuff like h2, handshakes, etc.. `env-filter` feature for tracing-subscriber would be helpful
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .finish();
    
    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default subscriber failed");

    // Initialize cfg, we use both .toml file and cli parameters for configuration
    // CLI parameters have higher priority than stryichain.toml so user may overlap values.
    let cfg = NodeConfig::load()
        .map_err(|e| {
            error!("Got error while trying to setup configuration : {e}");
            e
        })?;



    print_essentials();


    let sync_service_config = StryiSyncServiceConfig {
        address: cfg.grpc_sync_address.parse()?,
        chain_name:  cfg.chain_name,
        protocol_version: cfg.sync_protocol_version as usize,
        max_blocks_range_per_request: cfg.sync_max_blocks_per_request,
    };

    // Try to get genesis config by path
    let genesis_config = cfg.genesis_config_path
        .as_deref()
        .map(PathBuf::from)
        .map(try_genesis_config_from_path)
        .transpose()?;  

    
    // Initializing storage in configured provided path
    let storage = StryiStorage::initialize_in_path(PathBuf::from(cfg.storage_path), genesis_config).await?;
    let storage = Arc::new(RwLock::new(storage));



    // TODO: Improve mempool configurability, make possible configure FeePolicy, RbfPolicy
    let mempool_config = MemPoolConfig::new(
        cfg.mempool_max_transactions,
        FeePolicy::default(),
        RbfPolicy::default(), 
        36000
    );

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
    let peer_key = match PeerKey::restore(&cfg.peer_key_path) {
        Ok(k) => {
            info!("Restored peer key from {}", &cfg.peer_key_path);
            k
        }
        Err(_) => {
            info!("No existing peer key, generating a fresh one");
            let fresh = PeerKey::generate_random();
            // Ignore I/O error on first run; report only if backup fails later.
            let _ = fresh.backup(&cfg.peer_key_path);
            fresh
        }
    };

    let keypair = peer_key.inner().clone(); // clone to hand over to NetworkManager



    // generate tls identity for services of node
    let sans_vec: Vec<&str> = cfg.tls_sans.iter().map(String::as_str).collect();
    let tls_identity = cert_and_key_from_peer(&keypair, &sans_vec)
        .expect("Cannot generate certificate based on this peer's keypair");


    info!("Generated certificate for node services! This node certificate :");
    println!("{}", tls_identity.cert_pem.as_str().purple());


    let behaviour_config = StryiBehaviourConfig {
        ping_interval:  Duration::from_secs(cfg.network_ping_interval_secs),
        ping_timeout:   Duration::from_secs(cfg.network_ping_timeout_secs),
        gossipsub_heartbeat: Duration::from_secs(cfg.network_gossipsub_heartbeat_secs),
        enable_rendezvous_server: cfg.network_rendezvous_mode == "server",
        enable_rendezvous_client: cfg.network_rendezvous_mode == "client",
    };
    let network_manager_config = StryiNetworkManagerConfig {
        listen_addr: cfg.network_listen_addr,
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
