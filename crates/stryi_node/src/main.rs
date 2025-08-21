#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199


mod node;
mod grpc;
mod error;
mod keys;

/// Common middlewares for node's services
mod middleware;

/// Helper functions for generating x.509 certificates for node's services
mod tls;

/// Runtime configuration object for the node.
mod config;

/// Command-line overrides for node configuration.
mod cli;

/// High-level http api for users of the node.
mod http;

/// An implementation of node's mining service.
mod miner;

/// Simple estimation of the node's hashrate
mod hashrate;

/// Tools to let user choose genesis configuration (e.g. from file, another node, etc.)
mod genesis_manager;

use std::error::Error;
use std::io::{ErrorKind, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use colored::Colorize;
use tokio::io;
use tokio::time::sleep;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, EnvFilter};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use stryi_core::address::AccountAddress;
use stryi_core::mempool::{MemPool, MemPoolConfig, RbfPolicy, UtxoLookup};
use stryi_core::storage::{StorageStats, UtxoStorage};
use stryi_core::transactions::{FeePolicy, OutPoint};
use stryi_network::{StryiBehaviourConfig, StryiNetworkManager, StryiNetworkManagerConfig, RendezvousMode, ServiceInfo};
use stryi_storage::{GenesisInitConfig, StryiStorage};
use crate::cli::NodeStartMode;
use crate::grpc::{StryiSyncServiceConfig};
use crate::keys::PeerKey;
use crate::node::StryiChainNode;
use crate::tls::cert_and_key_from_peer;
use crate::config::NodeConfig;
use crate::error::StryiNodeError;
use crate::http::StryiHttpServiceConfig;
use crate::middleware::ReadyFlag;
use crate::miner::{StryiMiner, StryiMinerConfig};

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

    // Initialize the tracing subscriber.

    // Tracing subscriber for normal log output (filtering via env vars)
    let fmt_layer = fmt::layer()
        .with_target(true)
        .with_level(true);

    // Use EnvFilter to filter out some of unnecessary logs (like h2, handshakes, etc.)
    let filter_layer = EnvFilter::from_default_env()
        .add_directive("hyper=info".parse().unwrap())
        .add_directive("h2=info".parse().unwrap());


    // With telemetry enabled: include the console layer
    #[cfg(feature = "telemetry")]
    {
        let console_layer = console_subscriber::spawn();

        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .with(console_layer)
            .init();
    }

    // Without telemetry: omit the console layer entirely
    #[cfg(not(feature = "telemetry"))]
    {
        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .init();
    }


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
        chain_name:  cfg.chain_name.to_string(),
        protocol_version: cfg.sync_protocol_version as usize,
        max_blocks_range_per_request: cfg.sync_max_blocks_per_request,
    };

    let http_service_config = StryiHttpServiceConfig {
        address: cfg.http_service_address.parse()?,
        chain_name: cfg.chain_name,
        api_version: cfg.http_service_version,
    };


    let mut start_mode = cfg.start_mode;

    if matches!(start_mode, NodeStartMode::Auto) {
        start_mode = if cfg.genesis_config_path.is_some() {
            NodeStartMode::Bootstrap
        } else {
            NodeStartMode::Join
        }
    }
    info!("Start mode = {:?}", start_mode);

    // Try to get genesis config by path if we are in Bootstrap mode.
    let genesis_config = match start_mode {
        NodeStartMode::Bootstrap => {
            let p = cfg.genesis_config_path
                .as_deref()
                .ok_or_else(|| StryiNodeError::other("Bootstrap mode requires `genesis_config_path`"))?;
            Some(try_genesis_config_from_path(PathBuf::from(p))?)
        },
        NodeStartMode::Join => None,
        NodeStartMode::Auto => unreachable!(),
    };


    // Initializing storage in configured provided path
    let storage = StryiStorage::initialize_in_path(PathBuf::from(cfg.storage_path), genesis_config).await?;
    let storage = Arc::new(RwLock::new(storage));


    // TODO: Improve mempool configurability, make possible configure FeePolicy, RbfPolicy and set RbfPolicy::disabled from config
    let mempool_config = MemPoolConfig::new(
        cfg.mempool_max_transactions,
        FeePolicy::default(),
        RbfPolicy::disabled(), // Disable RBF for now
        60 * 60, // 1 hour expiry time
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
                storage.read().await.get_utxo(out_point).await.unwrap_or(None)
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


    // Generate TLS identity for node services.
    let mut sans_vec: Vec<&str> = cfg.tls_sans.iter().map(String::as_str).collect();

    // If the provided SAN list is empty, default to "localhost".
    if sans_vec.is_empty() {
        warn!("A custom SAN list was provided, but it was empty; defaulting to `localhost`.");
        sans_vec.push("localhost");
    }
    let tls_identity = cert_and_key_from_peer(&keypair, &sans_vec)
        .expect("Cannot generate certificate based on this peer's keypair");


    info!("Generated certificate for node services! This node certificate :");
    println!("{}", tls_identity.cert_pem.as_str().purple());


    let rendezvous_mode = match cfg.network_rendezvous_mode.as_str() {
        "server" => RendezvousMode::Server,
        "client" => RendezvousMode::Client,
        other => return Err(StryiNodeError::other(format!("invalid rendezvous mode: {}", other)).into()),
    };


    info!("Rendezvous mode is set to: {rendezvous_mode:?}");

    // Validate rendezvous server address if rendezvous mode is set to client
    if matches!(rendezvous_mode, RendezvousMode::Client)
        && cfg.network_rendezvous_address.as_deref().unwrap_or("").is_empty()
    {
        return Err(StryiNodeError::other("Client mode requires `network_rendezvous_address` to be provided").into());
    }


    let behaviour_config = StryiBehaviourConfig {
        ping_interval:  Duration::from_secs(cfg.network_ping_interval_secs),
        ping_timeout:   Duration::from_secs(cfg.network_ping_timeout_secs),
        gossipsub_heartbeat: Duration::from_secs(cfg.network_gossipsub_heartbeat_secs),
        enable_rendezvous_server: matches!(rendezvous_mode, RendezvousMode::Server),
        enable_rendezvous_client: matches!(rendezvous_mode, RendezvousMode::Client),
    };

    let network_manager_config = StryiNetworkManagerConfig {
        listen_addr: cfg.network_listen_addr,
        rendezvous_mode,
        rendezvous_server_addr: cfg.network_rendezvous_address.clone(),
        keypair: keypair.clone(),
        stryi_behaviour_config: behaviour_config,
        ..Default::default()
    };

    let network_manager_cancellation_token = CancellationToken::new();

    // An initially empty list – we'll fill it later when services start.
    let services_info: Arc<RwLock<Vec<ServiceInfo>>> = Arc::new(RwLock::new(Vec::new()));

    let network_manager = StryiNetworkManager::new(
        &network_manager_config,
        mempool.clone(),
        services_info.clone(),
        network_manager_cancellation_token)?;

    let network_manager = Some(network_manager);
    
    
    // Instantiate the StryiChainNode
    let mut node = StryiChainNode {
        mempool,
        storage,
        network_manager,
        services_info,
        tls_identity,
        net_cmd: None,
        net_events: None,
        sync_service_config,
        http_service_config,

        // These are temporary always set to true until I'll finish node's db synchronization
        grpc_is_ready: ReadyFlag::new(RwLock::new(true)),
        http_is_ready: ReadyFlag::new(RwLock::new(true)),
    };




    // -- Initialize the miner manager --
    let get_tip = {
        let storage = node.storage.clone();
        // Closure captures Arc-ed storage
        Box::new(move || {
            // Clone storage for the async block
            let storage = storage.clone();
            // Return boxed async future that reads tip
            Box::pin(async move { storage.read().await.tip().await })
        })
    };
    
    if cfg.miner_enabled {
        info!("Mining is enabled, initializing the miner...");

        let reward_address = if let Ok(addr) = AccountAddress::from_hash_string(&cfg.miner_reward_address) {
            addr
        } else {
            error!("Invalid miner reward address provided: {}", &cfg.miner_reward_address);
            return Err(StryiNodeError::other("Invalid miner reward address").into());
        };

        // Pretty print the miner reward address so user will not miss it
        println!("{}", "==================================MINER==================================".blue().bold());
        println!("{} {}", "Miner reward address is set to:".purple(), reward_address.to_string().green().bold());
        // Also print the hashrate (just for fun)
        hashrate::warm_up();
        println!("{}", "=========================================================================".blue().bold());




        // Create a channel for network commands
        // let (net_cmd_tx, net_cmd_rx) = tokio::sync::mpsc::channel(100);

        // Create a channel for network events
        // let (net_events_tx, net_events_rx) = tokio::sync::broadcast::channel(100);

        // Initialize the miner with the provided configuration
        let miner = StryiMinerConfig::new(
            cfg.miner_tx_threshold,
            cfg.block_header_version,
            cfg.miner_max_delay_secs,
            reward_address,
        );

        
        // Create the miner instance 
        // .....
        
        
        
    } else {
        info!("Mining is disabled, skipping miner initialization.");
    }




    // Connect the node to the network.
    // This will start the network manager and connect to the rendezvous server if configured.
    node.connect().await?;




    // Synchronize the node with the network.
    // This will fetch the latest blocks, transactions, and other data needed to bring the node


    match start_mode {
        NodeStartMode::Bootstrap => {
            info!("Bootstrap: skipping synchronize(); this node is the source of genesis.");
        },

        NodeStartMode::Join => {
            info!("Join: running synchronize() to fetch genesis/chain from peers.");
            node.synchronize().await?;
        }

        NodeStartMode::Auto => unreachable!(),
    }


    // node.synchronize().await?;


    // TODO: Keep back node.start_services() call later

    // Keep the node running indefinitely.
    loop {
        sleep(Duration::from_secs(60)).await;
    }
}
