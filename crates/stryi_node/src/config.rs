//! Strongly-typed, merged configuration for StryiNode.
//!
//! Precedence: hard-coded defaults < TOML file < explicit CLI flags.

use clap::Parser;
use serde::{Deserialize, Serialize};
use figment::{Figment, providers::{Toml, Serialized}};
use figment::providers::Format;
use crate::cli::CliArgs;
use crate::error::StryiNodeError;

#[derive(Debug, Serialize, Deserialize)]
pub struct NodeConfig {
    /* global settings */
    pub chain_name: String,
    pub genesis_config_path: Option<String>,

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

    /* mempool */
    pub mempool_max_transactions: usize,

    /* gRPC sync */
    pub grpc_sync_address: String,
    pub sync_protocol_version: u32,
    pub sync_max_blocks_per_request: usize,

    /* keys */
    pub peer_key_path: String,

    /* TLS */
    pub tls_sans: Vec<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            chain_name: "dev".into(),
            genesis_config_path: None,
                    
            network_listen_addr: "/ip4/0.0.0.0/tcp/1234".into(),
            network_rendezvous_mode: "server".into(),
            network_rendezvous_address: None,
            network_version: 1,
            network_rendezvous_namespace: "stryi-rendezvous".into(),
            network_ping_interval_secs: 10,
            network_ping_timeout_secs: 10,
            network_gossipsub_heartbeat_secs: 10,

            storage_path: "/var/lib/stryi_chain".into(),

            mempool_max_transactions: 100,

            grpc_sync_address: "0.0.0.0:5555".into(),
            sync_protocol_version: 1,
            sync_max_blocks_per_request: 100,

            peer_key_path: "/var/lib/stryi_chain/peer.stryi_keys".into(),

            tls_sans: vec!["localhost".into()],
        }
    }
}



impl NodeConfig {
    /// Merge defaults  <  TOML file  <  explicit CLI flags.
    pub fn load() -> Result<Self, StryiNodeError> {
        let cli = CliArgs::parse();

        let figment = Figment::new()
            // (3) built-in defaults, lowest priority
            .merge(Serialized::defaults(NodeConfig::default()))
            // (2) values from TOML, if the file exists
            .merge(Toml::file(&cli.config_path).profile("default"))
            // (1) explicit CLI flags – highest priority
            .merge(Serialized::from(cli, "default"));

        figment
            .extract()
            .map_err(|e| StryiNodeError::other(e))
    }
}
