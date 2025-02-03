use libp2p::identity::Keypair;

/// Indicates whether we run as a Rendezvous **Server** or a **Client** node.
#[derive(Debug, Clone)]
pub enum StryiNodeMode {
    /// Rendezvous Server (no block logic, just peer discovery)
    Server,
    /// Regular Rendezvous Client (connects to a server, discovers peers)
    Node,
}

/// Global config for a node: addresses, keypair, mode, etc.
#[derive(Debug, Clone)]
pub struct StryiNodeConfig {
    /// Which mode to run (Server or Node).
    pub mode: StryiNodeMode,

    /// Multiaddr to listen on, e.g. `/ip4/0.0.0.0/tcp/62649`.
    /// If you specify `/tcp/0` it picks a random port.
    pub listen_addr: String,

    /// If we are in Node mode, we can optionally dial a Rendezvous server,
    /// e.g. `/ip4/127.0.0.1/tcp/62649/p2p/<PEER_ID>`
    pub rendezvous_server_addr: Option<String>,

    /// Rendezvous namespace, e.g. `"stryichain"`.
    pub rendezvous_namespace: String,

    /// Optional identity key. If None, we generate a random Ed25519 key.
    pub keypair: Option<Keypair>,
}

impl Default for StryiNodeConfig {
    fn default() -> Self {
        Self {
            mode: StryiNodeMode::Node,
            listen_addr: "/ip4/0.0.0.0/tcp/0".to_string(),
            rendezvous_server_addr: None,
            rendezvous_namespace: "stryichain".to_string(),
            keypair: None,
        }
    }
}
