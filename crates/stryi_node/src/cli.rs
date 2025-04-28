//! Command-line overrides for StryiNode.
//!
//! Every field is an `Option<T>`.
//! If a flag is omitted, its value doesn’t overwrite the TOML file.

use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

#[skip_serializing_none]
#[derive(Parser, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
#[command(author, version, about = "StryiChain node")]
pub struct CliArgs {
    /// Path to the TOML configuration file.
    /// Only respected as a CLI flag; the key is ignored inside the file.
    #[arg(long, default_value = "stryichain.toml")]
    #[serde(skip)]
    pub config_path: String,

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

    /// Protocol version advertised via Identify.
    #[arg(long)]
    pub network_version: Option<u32>,

    /// Shared namespace string for rendezvous discovery.
    #[arg(long)]
    pub network_rendezvous_namespace: Option<String>,

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
    pub storage_path: Option<String>,

    /// Optional path to a JSON file describing the genesis block.
    #[arg(long)]
    pub genesis_config_path: Option<String>,

    /* mempool */

    /// Maximum number of transactions kept in memory.
    #[arg(long)]
    pub mempool_max_transactions: Option<usize>,

    /* gRPC sync */

    /// Socket address (`ip:port`) for the gRPC sync service.
    #[arg(long)]
    pub grpc_sync_address: Option<String>,

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

    /// Path where the peer Ed25519 key is backed up.
    #[arg(long)]
    pub peer_key_path: Option<String>,

    /* TLS */

    /// Comma-separated list of SANs for the self-signed certificate.
    #[arg(long)]
    pub tls_sans: Option<Vec<String>>,
}
