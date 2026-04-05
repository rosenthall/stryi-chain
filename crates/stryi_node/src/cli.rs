//! Command-line configuration overrides for StryiNode.
//!
//! Every field is an `Option<T>`.
//! If a flag is omitted, its value doesn’t overwrite the TOML file.

use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use std::{path::PathBuf, str::FromStr};

/// Represents the behavior of the node at startup.
#[derive(Debug, Deserialize, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")] // "bootstrap" | "join" | "auto"
pub enum NodeStartMode {
    /// the node starts from the provided genesis block and builds the chain from scratch.
    Bootstrap,

    /// the node connects to peer and takes its genesis block, and then builds the chain from there.
    Join,

    #[default]
    /// the node checks if it already has a genesis block, and if not, it starts in Bootstrap mode, else it starts in Join mode.
    Auto,
}

impl FromStr for NodeStartMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bootstrap" => Ok(NodeStartMode::Bootstrap),
            "join" => Ok(NodeStartMode::Join),
            "auto" => Ok(NodeStartMode::Auto),
            other => Err(format!("unknown StartMode: {other}")),
        }
    }
}

#[skip_serializing_none]
#[derive(Parser, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
#[command(name = "stryi-node", author, version, about = "StryiChain node")]
pub struct CliArgs {
    /// Path to the TOML configuration file.
    /// The node requires a TOML config file at startup.
    /// CLI flags override defaults and TOML settings, but do not make the config file optional.
    /// When omitted, the node tries `./stryichain.toml` and exits if it does not exist.
    #[arg(long)]
    #[serde(skip)]
    pub config_path: Option<PathBuf>,

    /* network */
    /// Multi-addr the node listens on, e.g. `/ip4/0.0.0.0/tcp/1234`.
    /// Use `/tcp/0` for an OS-assigned port.
    #[arg(long)]
    pub network_listen_addr: Option<String>,

    /// Rendezvous role: `"server"` or `"client"`.
    #[arg(long)]
    pub network_rendezvous_mode: Option<String>,

    /// Multi-addr of a rendezvous server (client mode only).
    #[arg(long)]
    pub network_rendezvous_address: Option<String>,

    /// Ping interval (seconds).
    #[arg(long)]
    pub network_ping_interval_secs: Option<u64>,

    /// Ping timeout (seconds).
    #[arg(long)]
    pub network_ping_timeout_secs: Option<u64>,

    /// Gossipsub heartbeat (seconds).
    #[arg(long)]
    pub network_gossipsub_heartbeat_secs: Option<u64>,

    /* storage */
    /// Directory containing the node’s database.
    #[arg(long)]
    pub storage_path: Option<PathBuf>,

    /// Optional path to a JSON file describing the genesis block.
    #[arg(long)]
    pub genesis_config_path: Option<PathBuf>,

    /// Automatically accept the provided genesis without interactive confirmation.
    #[arg(long)]
    pub auto_accept_genesis: Option<bool>,

    /* mempool */
    /// Maximum number of transactions kept in memory.
    #[arg(long)]
    pub mempool_max_transactions: Option<usize>,

    /* miner */
    /// Enable the miner.
    #[arg(long)]
    pub miner_enabled: Option<bool>,

    /// Enable the hashrate bench in the node startup.
    #[arg(long)]
    pub miner_hashrate_bench: Option<bool>,

    /// Target number of transactions before mining.
    #[arg(long)]
    pub miner_tx_threshold: Option<usize>,

    /// Maximum delay (seconds) before mining a block, even if the tx threshold is not reached.
    #[arg(long)]
    pub miner_max_delay_secs: Option<usize>,

    /// Block reward receiver address
    #[arg(long)]
    pub miner_reward_address: Option<String>,

    /* gRPC sync */
    /// Socket address (`ip:port`) for the gRPC sync service.
    #[arg(long)]
    pub grpc_sync_listen: Option<String>,

    /// Human-readable chain name (exposed via sync).
    #[arg(long)]
    pub sync_chain_name: Option<String>,

    /// Sync-protocol version.
    #[arg(long)]
    pub sync_protocol_version: Option<u32>,

    /// Upper bound on blocks served per request.
    #[arg(long)]
    pub sync_max_blocks_per_request: Option<usize>,

    /* keys */
    /// Path where the peer's Ed25519 key is backed up.
    #[arg(long)]
    pub peer_key_path: Option<PathBuf>,

    /* TLS */
    /// Comma-separated list of SANs for the self-signed certificate.
    #[arg(long)]
    pub tls_sans: Option<Vec<String>>,
}
