use crate::error::StryiNodeError;
use multiaddr::{Multiaddr, Protocol};
use std::io::{ErrorKind, Read};
use std::net::SocketAddrV4;
use std::path::PathBuf;
use stryi_storage::GenesisInitConfig;
use tokio::io;

/// Resolves IPr4 advertise address from config or infers from listen address
pub fn resolve_ipv4_advertise(
    advertise_opt: Option<String>,
    listen: &str,
    service_name: &str,
) -> Result<Multiaddr, String> {
    // Explicit advertise wins
    if let Some(s) = advertise_opt {
        return s
            .parse::<Multiaddr>()
            .map_err(|e| format!("invalid {} advertise address: {e}", service_name));
    }

    // Parse listen as SocketAddrV4
    let listen_v4: SocketAddrV4 = listen.parse().map_err(|_| {
        format!(
            "{} listen must be IPv4 socket addr, got: {}",
            service_name, listen
        )
    })?;

    let ip = *listen_v4.ip();
    let port = listen_v4.port();

    // Reject unspecified IPs
    if ip.is_unspecified() {
        return Err(format!(
            "{} advertise not set and listen IP is 0.0.0.0 — \
             you MUST set {}_advertise explicitly",
            service_name, service_name
        ));
    }

    // Build Multiaddr
    Ok(Multiaddr::empty()
        .with(Protocol::Ip4(ip))
        .with(Protocol::Tcp(port)))
}

/// Reads and deserializes the config from a provided path.
pub fn try_genesis_config_from_path(path: PathBuf) -> Result<GenesisInitConfig, StryiNodeError> {
    // Check if file exists and if it is a file.
    if !path.is_file() {
        return Err(StryiNodeError::Io(io::Error::new(
            ErrorKind::NotFound,
            format!(
                "Provided path with genesis configuration is not a file or doesn't exists. Path : {}",
                path.display()
            ),
        )));
    }

    let mut file = std::fs::File::open(&path)?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;

    // Try to deserialize
    serde_json::from_str(&buf).map_err(|e| {
        StryiNodeError::other(format!(
            "Cannot deserialize genesis configuration, error : {}",
            e
        ))
    })
}
