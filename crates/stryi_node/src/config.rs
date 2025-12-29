//! Strongly-typed, merged configuration for StryiNode.
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
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeConfig {
    /* global settings */
    pub chain_name: String,
    pub chain_id: String,
    pub genesis_config_path: Option<String>,
    pub block_header_version: u16,
    #[serde(default)]
    pub start_mode: NodeStartMode,

    /* network */
    pub network_listen_addr: String,
    pub network_rendezvous_mode: String,
    pub network_rendezvous_address: Option<String>,
    pub network_version: u32,
    pub network_rendezvous_namespace: String,
    pub network_ping_interval_secs: u64,
    pub network_ping_timeout_secs: u64,
    pub network_gossipsub_heartbeat_secs: u64,

    /* storage */
    pub storage_path: String,
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
    pub grpc_sync_address: String,
    pub sync_protocol_version: u32,
    pub sync_max_blocks_per_request: usize,

    /* http service */
    pub http_service_address: String,
    pub http_service_version: u32,

    /* keys */
    pub peer_key_path: String,

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
            network_version: 1,
            network_rendezvous_namespace: "stryi-rendezvous".into(),
            network_ping_interval_secs: 10,
            network_ping_timeout_secs: 10,
            network_gossipsub_heartbeat_secs: 10,

            storage_path: "/var/lib/stryi_chain".into(),
            auto_accept_genesis: false,

            mempool_max_transactions: 100,

            miner_enabled: false,
            miner_hashrate_bench: false,
            miner_tx_threshold: 1,
            miner_max_delay_secs: 20,
            miner_reward_address: "@nah".to_string(),

            grpc_sync_address: "0.0.0.0:5555".into(),
            sync_protocol_version: 1,
            sync_max_blocks_per_request: 100,

            http_service_address: "0.0.0.0:5556".to_string(),
            http_service_version: 1,

            peer_key_path: "/var/lib/stryi_chain/peer.stryi_keys".into(),

            tls_sans: vec!["localhost".into()],
        }
    }
}

impl NodeConfig {
    /// Merge defaults  <  TOML file  <  explicit CLI flags.
    /// Will return error if config_path was provided in CLI parameters but does not exist
    pub fn load() -> Result<Self, StryiNodeError> {
        let cli = CliArgs::parse();

        // assert that config_path exists
        let path = Path::new(&cli.config_path);
        if !path.is_file() {
            return Err(StryiNodeError::Other(format!(
                "The provided config path is not a file! Provided path : {}",
                path.to_str().unwrap()
            )));
        }

        let figment = Figment::new()
            // (3) built-in defaults, lowest priority
            .merge(Serialized::defaults(NodeConfig::default()))
            // (2) values from TOML, if the file exists
            .merge(Toml::file(&cli.config_path).profile("default"))
            // (1) explicit CLI flags – highest priority
            .merge(Serialized::from(cli, "default"));

        figment.extract().map_err(StryiNodeError::other)
    }
}
