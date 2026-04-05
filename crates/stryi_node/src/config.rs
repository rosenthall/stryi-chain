//! Strongly typed, merged configuration for StryiNode.
//!
//! Precedence: hard-coded defaults < TOML file < explicit CLI flags.

use crate::cli::{CliArgs, NodeStartMode};
use crate::error::StryiNodeError;
use clap::Parser;
use figment::providers::Format;
use figment::{
    Figment,
    providers::{Serialized, Toml},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_PATH: &str = "stryichain.toml";

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeConfig {
    /* global settings */
    pub chain_name: String,
    pub chain_id: String,
    pub genesis_config_path: Option<PathBuf>,
    pub block_header_version: u16,
    #[serde(default)]
    pub start_mode: NodeStartMode,

    /* network */
    pub network_listen_addr: String,
    pub network_rendezvous_mode: String,
    pub network_rendezvous_address: Option<String>,
    pub network_ping_interval_secs: u64,
    pub network_ping_timeout_secs: u64,
    pub network_gossipsub_heartbeat_secs: u64,

    /* storage */
    pub storage_path: PathBuf,
    pub auto_accept_genesis: bool,

    /* mempool */
    pub mempool_max_transactions: usize,

    /* miner */
    pub miner_enabled: bool,
    pub miner_hashrate_bench: bool,
    pub miner_tx_threshold: usize,
    pub miner_max_delay_secs: usize,
    pub miner_reward_address: String,

    /* gRPC sync */
    pub grpc_sync_listen: String,
    pub grpc_sync_advertise: Option<String>,
    pub sync_protocol_version: u32,
    pub sync_max_blocks_per_request: usize,

    /* http service */
    pub http_service_listen: String,
    pub http_service_advertise: Option<String>,
    pub http_service_version: u32,

    /* keys */
    pub peer_key_path: PathBuf,

    /* TLS */
    pub tls_sans: Vec<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            chain_name: "dev".into(),
            chain_id: "devnet-001".to_string(),
            genesis_config_path: None,
            block_header_version: 1,

            start_mode: NodeStartMode::Auto,
            network_listen_addr: "/ip4/0.0.0.0/tcp/1234".into(),
            network_rendezvous_mode: "server".into(),
            network_rendezvous_address: None,
            network_ping_interval_secs: 10,
            network_ping_timeout_secs: 10,
            network_gossipsub_heartbeat_secs: 10,

            storage_path: PathBuf::new(),
            auto_accept_genesis: false,

            mempool_max_transactions: 100,

            miner_enabled: false,
            miner_hashrate_bench: false,
            miner_tx_threshold: 1,
            miner_max_delay_secs: 20,
            miner_reward_address: "@nah".to_string(),

            grpc_sync_listen: "0.0.0.0:5555".into(),
            grpc_sync_advertise: None,
            sync_protocol_version: 1,
            sync_max_blocks_per_request: 100,

            http_service_listen: "0.0.0.0:5556".to_string(),
            http_service_advertise: None,
            http_service_version: 1,

            peer_key_path: PathBuf::new(),

            tls_sans: vec!["localhost".into()],
        }
    }
}

impl NodeConfig {
    /// Merge defaults  <  TOML file  <  explicit CLI flags.
    /// Requires a TOML config file either via `--config-path` or `./stryichain.toml`.
    pub fn load() -> Result<Self, StryiNodeError> {
        let cli = CliArgs::parse();
        let storage_path_from_cli = cli.storage_path.is_some();
        let peer_key_path_from_cli = cli.peer_key_path.is_some();
        let genesis_config_path_from_cli = cli.genesis_config_path.is_some();

        let config_path = match cli.config_path.as_deref() {
            Some(path) => {
                let path = path.to_path_buf();
                if !path.is_file() {
                    return Err(StryiNodeError::other(format!(
                        "The provided config path is not a file! Provided path: {}",
                        path.display()
                    )));
                }
                path
            }
            // Fallback to the default config path if no config path is provided.
            None => {
                let path = PathBuf::from(DEFAULT_CONFIG_PATH);
                if !path.is_file() {
                    return Err(StryiNodeError::other(format!(
                        "No configuration file found. Pass --config-path or create ./{DEFAULT_CONFIG_PATH}.",
                    )));
                }
                path
            }
        };

        let config_dir = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();

        let mut figment = Figment::new()
            // built-in defaults have the lowest priority
            .merge(Serialized::defaults(NodeConfig::default()));

        figment = figment.merge(Toml::file(&config_path).profile("default"));
        figment = figment.merge(Serialized::from(cli, "default"));

        let mut cfg: Self = figment.extract().map_err(StryiNodeError::other)?;

        if !genesis_config_path_from_cli {
            resolve_optional_path_from_config_dir(&mut cfg.genesis_config_path, &config_dir);
        }

        if !storage_path_from_cli {
            resolve_path_from_config_dir(&mut cfg.storage_path, &config_dir);
        }
        if cfg.storage_path.as_os_str().is_empty() {
            return Err(StryiNodeError::invalid_config_value(
                "storage_path is required. Set it in the config file or pass --storage-path.",
            ));
        }

        if !peer_key_path_from_cli {
            resolve_path_from_config_dir(&mut cfg.peer_key_path, &config_dir);
        }
        if cfg.peer_key_path.as_os_str().is_empty() {
            cfg.peer_key_path = cfg.storage_path.join("peer.stryi_keys");
        }

        Ok(cfg)
    }
}

fn resolve_optional_path_from_config_dir(path: &mut Option<PathBuf>, config_dir: &Path) {
    if let Some(path) = path {
        resolve_path_from_config_dir(path, config_dir);
    }
}

fn resolve_path_from_config_dir(path: &mut PathBuf, config_dir: &Path) {
    if path.as_os_str().is_empty() {
        return;
    }

    let current_path = path.clone();
    if current_path.is_relative() {
        *path = config_dir.join(current_path);
    }
}
