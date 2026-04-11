#![allow(incomplete_features)]
#![feature(generic_const_exprs)] // This feature was added to avoid a known bug: https://github.com/rust-lang/rust/issues/133199

/// Node & EventLoop implementation
mod node;

/// error type for his crate
mod error;

/// grpc server implementation
mod grpc;

/// Helpers for backing up the keys of the peer.
mod keys;

/// Common middlewares for node's services
mod middleware;

/// Helper functions for generating x.509 certificates for node's services
mod tls;

/// Runtime configuration object for the node.
mod config;

/// Command-line overrides for node configuration.
mod cli;

/// Some util functions used in main()
mod util;

/// High-level http api for users of the node.
mod http;

/// An implementation of node's mining service.
mod miner;

/// Simple estimation of the node's hashrate
mod hashrate;

/// Tools for proper bootstrapping of the chain and genesis acquiring
mod bootstrap;

use crate::bootstrap::GenesisBootstrap;
use crate::cli::NodeStartMode;
use crate::config::NodeConfig;
use crate::error::StryiNodeError;
use crate::grpc::StryiSyncServiceConfig;
use crate::http::StryiHttpServiceConfig;
use crate::keys::PeerKey;
use crate::middleware::ready::ReadyFlag;
use crate::miner::{MinerBackend, NodeMinerBackend, StryiMiner, StryiMinerConfig};
use crate::node::miner_bridge::MinerBridge;
use crate::node::sync::build_consensus_constants;
use crate::node::{EventLoopContext, StryiChainNode};
use crate::tls::cert_and_key_from_peer;
use crate::util::{derive_grpc_tls_sans, resolve_ipv4_advertise, try_genesis_config_from_path};
use colored::Colorize;
use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use stryi_core::address::AccountAddress;
use stryi_core::block::Block;
use stryi_core::mempool::{MemPool, MemPoolConfig, RbfPolicy, UtxoLookup};
use stryi_core::storage::UtxoStorage;
use stryi_core::transactions::{FeePolicy, OutPoint};
use stryi_network::{
    PeerId, RendezvousMode, StryiBehaviourConfig, StryiNetworkManager, StryiNetworkManagerConfig,
};
use stryi_storage::{StorageStatus, StryiStorage};
use tokio::sync::broadcast;
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, trace};
use tracing_subscriber::layer::{Layer, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

pub(crate) mod grpc_services {
    tonic::include_proto!("stryi.sync");
}

#[cfg(all(feature = "telemetry", not(tokio_unstable)))]
compile_error!("stryi-node's `telemetry` feature requires RUSTFLAGS=\"--cfg tokio_unstable\"");

fn print_essentials() {
    println!("{}", "Welcome to the StryiChain Node CLI !".bright_yellow());
    println!("- Node version: {}", env!("CARGO_PKG_VERSION").green());
    println!("- Description: {}", env!("CARGO_PKG_DESCRIPTION").white());
    println!("My {}: https://github.com/rosenthall", "GitHub".green());
    println!(
        "StryiChain {} repository: https://github.com/rosenthall/stryi-chain/",
        "GitHub".green()
    );

    println!(
        "{}{}",
        r#"
    █▀▀ ▀█▀ █▀█ ▀▄▀ ▀█▀  █▀▀ █▄█ ▄▀▄ ▀█▀ █▄ █
    ▄██  █  █▀▄  █  ▄█▄  █▄▄ █ █ █▀█ ▄█▄ █ ▀█
    "#
        .blue(),
        r#"
                █▄ █ █▀█ █▀▄ █▀▀
                █ ▀█ █▄█ █▄▀ ██▄
    "#
        .green()
        .on_black()
    );

    println!("{}", "Starting..".blink().green());
}

fn use_json_logs() -> bool {
    matches!(
        std::env::var("STRYI_LOG_FORMAT").ok().as_deref(),
        Some("json") | Some("JSON")
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Use EnvFilter to filter out some of the unnecessary logs (like h2, handshakes, etc.)
    // Set the default log level to info if RUST_LOG is not set
    let filter_layer = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("hyper=info".parse()?)
        .add_directive("h2=info".parse()?)
        .add_directive("lsm_tree=info".parse()?);

    // With telemetry enabled we include the console layer
    #[cfg(feature = "telemetry")]
    {
        if use_json_logs() {
            tracing_subscriber::registry()
                .with(
                    fmt::layer()
                        .json()
                        .with_target(true)
                        .with_level(true)
                        .flatten_event(true)
                        .with_filter(filter_layer.clone()),
                )
                .with(console_subscriber::spawn())
                .init();
        } else {
            tracing_subscriber::registry()
                .with(
                    fmt::layer()
                        .with_target(true)
                        .with_level(true)
                        .with_filter(filter_layer.clone()),
                )
                .with(console_subscriber::spawn())
                .init();
        }
    }

    // Without telemetry, we omit the console layer entirely
    #[cfg(not(feature = "telemetry"))]
    {
        if use_json_logs() {
            tracing_subscriber::registry()
                .with(
                    fmt::layer()
                        .json()
                        .with_target(true)
                        .with_level(true)
                        .flatten_event(true)
                        .with_filter(filter_layer.clone()),
                )
                .init();
        } else {
            tracing_subscriber::registry()
                .with(
                    fmt::layer()
                        .with_target(true)
                        .with_level(true)
                        .with_filter(filter_layer),
                )
                .init();
        }
    }

    let cfg = NodeConfig::load().map_err(|e| {
        error!("Got error while trying to setup configuration : {e}");
        e
    })?;

    print_essentials();

    let sync_service_config = StryiSyncServiceConfig {
        address: cfg.grpc_sync_listen.parse()?,
        chain_name: cfg.chain_name.to_string(),
        protocol_version: cfg.sync_protocol_version as usize,
        max_blocks_range_per_request: cfg.sync_max_blocks_per_request,
    };

    let mut http_service_config = StryiHttpServiceConfig {
        address: cfg.http_service_listen.parse()?,
        chain_name: cfg.chain_name.clone(),
        peer_id: PeerId::random(), // Setup it later
        api_version: cfg.http_service_version,
    };

    let grpc_advertise = resolve_ipv4_advertise(
        cfg.grpc_sync_advertise.clone(),
        &cfg.grpc_sync_listen,
        "grpc_sync",
    )
    .map_err(StryiNodeError::invalid_config_value)?;

    let http_advertise = resolve_ipv4_advertise(
        cfg.http_service_advertise.clone(),
        &cfg.http_service_listen,
        "http_service",
    )
    .map_err(StryiNodeError::invalid_config_value)?;

    let mut start_mode = cfg.start_mode;

    if matches!(start_mode, NodeStartMode::Auto) {
        start_mode = if cfg.genesis_config_path.is_some() {
            NodeStartMode::Bootstrap
        } else {
            NodeStartMode::Join
        }
    }
    info!("Start mode = {:?}", start_mode);

    // Setup bootstrap helper
    let genesis_bootstrap = GenesisBootstrap::new(cfg.storage_path.clone());

    // Initializing storage in the configured provided path

    // probe storage's meta-information
    let storage_status = StorageStatus::from_path(&cfg.storage_path).map_err(|e| {
        error!(
            "Failed to probe storage at {}: {e}",
            cfg.storage_path.display()
        );
        StryiNodeError::other(format!("probe storage: {e}"))
    })?;
    info!(
        "Storage status at {} => {:?}",
        cfg.storage_path.display(),
        storage_status
    );

    let storage: Arc<RwLock<StryiStorage>> = match (start_mode, &storage_status) {
        // if already initialized - just open
        (_start_mode, StorageStatus::Initialized { .. }) => {
            let st = StryiStorage::initialize_in_path(cfg.storage_path.clone(), None).await?;
            Arc::new(RwLock::new(st))
        }

        // if doing bootstrap and datadir is empty - setup meta
        (NodeStartMode::Bootstrap, StorageStatus::NoGenesis) => {
            let p = cfg.genesis_config_path.as_ref().ok_or_else(|| {
                StryiNodeError::invalid_config_value(
                    "Bootstrap mode requires `genesis_config_path`",
                )
            })?;
            let genesis_cfg = try_genesis_config_from_path(p)?;

            let preview_block = Block::new_genesis(
                genesis_cfg.version,
                HashMap::from_iter(genesis_cfg.wanted_balances.clone()),
                genesis_cfg.genesis_state,
            );

            genesis_bootstrap
                .clone()
                .confirm_and_save(
                    &preview_block,
                    &cfg.chain_name,
                    cfg.sync_protocol_version as u64,
                    Some("local"),
                    cfg.auto_accept_genesis,
                )
                .map_err(|e| {
                    error!("confirm_and_save failed: {e}");
                    e
                })?;

            let st = StryiStorage::initialize_in_path(cfg.storage_path.clone(), Some(genesis_cfg))
                .await?;
            Arc::new(RwLock::new(st))
        }

        // if Join and datadir is empty it's a pre-Genesis layout
        // genesis will be fetched and saved later.
        (NodeStartMode::Join, StorageStatus::NoGenesis) => {
            let st = StryiStorage::initialize_in_path(cfg.storage_path.clone(), None).await?;
            Arc::new(RwLock::new(st))
        }

        // if meta is corrupted - halt
        (_, StorageStatus::Corrupted { reason }) => {
            return Err(StryiNodeError::other(format!("Storage meta corrupted: {reason}")).into());
        }

        _ => panic!("unexpected (start_mode, storage_status) state"),
    };

    // TODO: Improve mempool configurability, make possible configure FeePolicy, RbfPolicy and set RbfPolicy::disabled from config
    let mempool_config = MemPoolConfig::new(
        cfg.mempool_max_transactions,
        FeePolicy::default(),
        RbfPolicy::disabled(), // Disable RBF for now
        60 * 60,               // 1-hour expiry time
    );

    // Create utxo_lookup closure for mempool that reads UTXO by the outpoint from storage
    let utxo_lookup: UtxoLookup = {
        let storage = storage.clone();

        Box::new(move |out_point: &OutPoint| {
            let storage = storage.clone();
            let out_point = *out_point;

            Box::pin(async move {
                storage
                    .read()
                    .await
                    .get_utxo(out_point)
                    .await
                    .unwrap_or(None)
            })
        })
    };

    let mempool = Arc::new(RwLock::new(MemPool::new(mempool_config, utxo_lookup)));

    // -- Initialize NetworkManager --

    let peer_key = PeerKey::restore_or_generate(&cfg.peer_key_path)?;
    let keypair = peer_key.inner().clone();

    // -- get peer_id --
    let peer_id = PeerId::from_public_key(&keypair.public());
    http_service_config.peer_id = peer_id;
    trace!("This node's peer id from keypair: {}", peer_id);

    // Generate TLS identity for node services from the advertised gRPC host plus extra SANs.
    let tls_sans = derive_grpc_tls_sans(&grpc_advertise, &cfg.tls_sans)
        .map_err(StryiNodeError::invalid_config_value)?;
    let sans_vec: Vec<&str> = tls_sans.iter().map(String::as_str).collect();
    let tls_identity = cert_and_key_from_peer(&keypair, &sans_vec)
        .expect("Cannot generate certificate based on this peer's keypair");

    info!("Generated certificate for node services! This node certificate :");
    println!("{}", tls_identity.cert_pem.as_str().purple());

    let rendezvous_mode = match cfg.network_rendezvous_mode.as_str() {
        "server" => RendezvousMode::Server,
        "client" => RendezvousMode::Client,
        other => {
            return Err(StryiNodeError::invalid_config_value(format!(
                "invalid rendezvous mode: {}",
                other
            ))
            .into());
        }
    };

    info!("Rendezvous mode is set to: {rendezvous_mode:?}");

    // Validate rendezvous server address if rendezvous mode is set to client
    if matches!(rendezvous_mode, RendezvousMode::Client)
        && cfg
            .network_rendezvous_address
            .as_deref()
            .unwrap_or("")
            .is_empty()
    {
        return Err(StryiNodeError::invalid_config_value(
            "Client mode requires `network_rendezvous_address` to be provided",
        )
        .into());
    }

    let behaviour_config = StryiBehaviourConfig {
        ping_interval: Duration::from_secs(cfg.network_ping_interval_secs),
        ping_timeout: Duration::from_secs(cfg.network_ping_timeout_secs),
        gossipsub_heartbeat: Duration::from_secs(cfg.network_gossipsub_heartbeat_secs),
    };

    let network_manager_config = StryiNetworkManagerConfig {
        listen_addr: cfg.network_listen_addr.clone(),
        rendezvous_mode,
        rendezvous_server_addr: cfg.network_rendezvous_address.clone(),
        keypair: keypair.clone(),
        stryi_behaviour_config: behaviour_config,
    };

    // build a channel for tip updates.
    let (tip_updates_sender, tip_updates_receiver) = broadcast::channel(1);

    // Create a channel, in which Miner will be sending blocks once found nonce,
    // so nonce can process it (apply, or propagate to other nodes)
    let (mined_blocks_sender, mined_blocks_receiver) = mpsc::channel(4);

    // Master cancellation token, it triggers by SIGINT/SIGTERM signals
    // All subsystems receive child tokens derived from this one
    let master_cancel_token = CancellationToken::new();

    let network_manager = StryiNetworkManager::new(
        &network_manager_config,
        mempool.clone(),
        master_cancel_token.child_token(),
    )?;

    // -- Validate miner config (before connecting) --

    let miner_config = if cfg.miner_enabled {
        info!("Mining is enabled, validating miner configuration...");

        // Try to get a reward address
        let reward_address =
            if let Ok(addr) = AccountAddress::from_hash_string(&cfg.miner_reward_address) {
                addr
            } else {
                error!(
                    "Invalid miner reward address provided: {}",
                    &cfg.miner_reward_address
                );
                return Err(
                    StryiNodeError::invalid_config_value("Invalid miner reward address").into(),
                );
            };

        // Pretty print the miner reward address so the user will not miss it
        println!(
            "{}",
            "==================================MINER=================================="
                .blue()
                .bold()
        );
        println!(
            "{} {}",
            "Miner reward address is set to:".purple(),
            reward_address.to_string().green().bold()
        );

        // Run the hashrate bench if enabled in config
        if cfg.miner_hashrate_bench {
            hashrate::warm_up();
        }

        println!(
            "{}",
            "========================================================================="
                .blue()
                .bold()
        );

        Some(StryiMinerConfig::new(
            cfg.miner_tx_threshold,
            cfg.block_header_version,
            cfg.miner_max_delay_secs,
            reward_address,
        ))
    } else {
        info!("Mining is disabled, skipping miner initialization.");
        None
    };

    // Optionally build the miner bridge
    let miner_bridge = miner_config.as_ref().map(|_mc| MinerBridge {
        mined_blocks_receiver,
    });

    // Build the node
    let node = StryiChainNode::new(
        storage.clone(),
        mempool.clone(),
        network_manager,
        genesis_bootstrap.clone(),
        sync_service_config,
    );

    // Connect the node to the network.
    let node = node.connect().await?;

    // Synchronize the node with the network.
    // Depending on the start mode, this may involve fetching the genesis block and chain data from peers.
    // Or if bootstrapping, skip synchronization as this node is the source of genesis.
    let node = match start_mode {
        NodeStartMode::Bootstrap => node.bootstrap().await?,
        NodeStartMode::Join => {
            info!("Join: running synchronize() to fetch genesis/chain from peers.");
            node.synchronize().await?
        }
        NodeStartMode::Auto => unreachable!(),
    };

    // -- Spawn the miner if enabled --
    // This must happen after connect() (thus channels exist) and after consensus engine init (thus difficulty calc available).
    if let Some(miner_cfg) = miner_config {
        info!("Spawning miner...");

        let consensus_consts = build_consensus_constants(&storage).await?;

        let backend: Arc<dyn MinerBackend> = Arc::new(NodeMinerBackend {
            storage: storage.clone(),
            consensus_consts,
            tip_updates: tip_updates_receiver,
        });

        let miner = StryiMiner::new(
            miner_cfg,
            mempool.clone(),
            backend,
            mined_blocks_sender,
            master_cancel_token.child_token(),
        );

        miner.spawn();
        info!("Miner spawned successfully!");
    }

    // Spawn a signal listener that cancels the master token on SIGINT/SIGTERM.
    {
        let cancel = master_cancel_token.clone();
        tokio::task::Builder::new()
            .name("signal-listener")
            .spawn(async move {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        info!("Received SIGINT, initiating graceful shutdown...");
                    }
                    _ = sigterm.recv() => {
                        info!("Received SIGTERM, initiating graceful shutdown...");
                    }
                }
                cancel.cancel();
            })
            .expect("failed to spawn signal-listener task");
    }

    // Build the event loop context
    let ctx = EventLoopContext {
        tip_updates_sender,
        miner_bridge,
        tls_identity,
        http_service_config,
        grpc_is_ready: ReadyFlag::new(RwLock::new(false)),
        http_is_ready: ReadyFlag::new(RwLock::new(false)),
        http_advertise_address: http_advertise,
        grpc_advertise_address: grpc_advertise,
        cancel_token: master_cancel_token.child_token(),
    };

    // Transition into the event loop and run until canceled
    let event_loop = node.into_event_loop(ctx);
    event_loop.run().await?;
    /* now it blokchainin'*/

    info!("Services stopped. Flushing storage...");
    if let Err(e) = storage.read().await.persist() {
        error!("Failed to flush storage on shutdown: {e:?}");
    } else {
        info!("Storage flushed successfully.");
    }

    info!("Node shutdown complete.");
    Ok(())
}
