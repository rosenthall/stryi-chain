use crate::error::StryiNodeError;
use multiaddr::{Multiaddr, Protocol};
use rustls_pki_types::ServerName;
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
            "{} advertise address not set and listen IP is 0.0.0.0 - \
             you MUST set {}_advertise explicitly",
            service_name, service_name
        ));
    }

    // Build Multiaddr
    Ok(Multiaddr::empty()
        .with(Protocol::Ip4(ip))
        .with(Protocol::Tcp(port)))
}

/// Extracts the host part that should be used for TLS verification from a service multiaddr.
pub fn extract_tls_verification_host(addr: &Multiaddr) -> Result<String, String> {
    for protocol in addr.iter() {
        match protocol {
            Protocol::Dns4(host) => return Ok(host.to_string()),
            Protocol::Ip4(ip) => return Ok(ip.to_string()),
            _ => {}
        }
    }

    Err("multiaddr missing host (dns4/ip4)".into())
}

/// Builds the SAN list for the gRPC server certificate from the advertised host plus extra config.
pub fn derive_grpc_tls_sans(
    grpc_advertise: &Multiaddr,
    extra_sans: &[String],
) -> Result<Vec<String>, String> {
    let advertised_host = extract_tls_verification_host(grpc_advertise)?;
    ServerName::try_from(advertised_host.as_str())
        .map_err(|e| format!("invalid TLS verification host '{advertised_host}': {e}"))?;

    let mut sans = vec![advertised_host];
    for san in extra_sans {
        if !sans.iter().any(|existing| existing == san) {
            sans.push(san.clone());
        }
    }

    Ok(sans)
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

#[cfg(test)]
mod tests {
    use super::{derive_grpc_tls_sans, extract_tls_verification_host};

    #[test]
    fn extracts_tls_verification_host_from_dns4_and_ip4() {
        let dns4: multiaddr::Multiaddr = "/dns4/server-node/tcp/2080".parse().unwrap();
        let ip4: multiaddr::Multiaddr = "/ip4/127.0.0.1/tcp/6001".parse().unwrap();

        assert_eq!(
            extract_tls_verification_host(&dns4).unwrap(),
            "server-node".to_string()
        );
        assert_eq!(
            extract_tls_verification_host(&ip4).unwrap(),
            "127.0.0.1".to_string()
        );
    }

    #[test]
    fn derives_tls_sans_from_advertise_host_and_extras() {
        let advertise: multiaddr::Multiaddr = "/dns4/server-node/tcp/2080".parse().unwrap();
        let sans = derive_grpc_tls_sans(
            &advertise,
            &[
                "localhost".into(),
                "server-node".into(),
                "grpc.internal".into(),
            ],
        )
        .unwrap();

        assert_eq!(
            sans,
            vec![
                "server-node".to_string(),
                "localhost".to_string(),
                "grpc.internal".to_string()
            ]
        );
    }

    #[test]
    fn rejects_invalid_tls_verification_host() {
        let advertise: multiaddr::Multiaddr = "/dns4/bad host/tcp/2080".parse().unwrap();
        let err = derive_grpc_tls_sans(&advertise, &[]).unwrap_err();
        assert!(err.contains("invalid TLS verification host"));
    }
}
