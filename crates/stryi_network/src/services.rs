//! This file defines our RequestResponse-based custom behaviour for obtaining up-to-date information about the peer's services
use std::net::SocketAddr;
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde::{Deserialize, Serialize};
use libp2p::request_response::cbor::{Behaviour as RequestResponseBehaviour};
use libp2p::request_response::{Message, Event as ReqRespEvent};
use crate::ed25519::PublicKey;
use crate::StryiEvent;

/// Requests enum for ServicesInfo api
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ServicesInfoRequest {
    ListServices,
}


/// Response type: list of services
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServicesResponse {
    pub services: Vec<ServiceInfo>,
}



/// ServiceInfo defines information we can gather about service(like gRPC api, json-rpc, etc.) which is running on some node/peer.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ServiceInfo {
    /// The kind of service e.g. "grpc", "jsonrpc", "graphql".
    kind : String,

    /// Address(ip:port) in human-readable format e.g. 37.73.37.73:12240
    address : SocketAddr,

    /// Version of service to define if service is compatible with this node version.
    version: u32,
    
    /// PEM string of TLS certificate used by the service.
    #[serde(skip_serializing_if = "Option::is_none")]
    cert_pem: Option<String>,

    /// Signature of the certificate by the node's private key, to prove ownership of the service.
    #[serde(skip_serializing_if = "Option::is_none")]
    cert_sig: Option<String>
}




impl ServiceInfo {
    /// Construct a new ServiceInfo
    pub fn new(
        kind: String,
        address: SocketAddr,
        version: u32,
        cert_pem: Option<String>,
        cert_sig: Option<String>
    ) -> Self {
        ServiceInfo {
            kind,
            address,
            version,
            cert_pem,
            cert_sig
        }
    }

    
    /// Construct a new ServiceInfo with signed certificate
    /// The certificate is signed by the node's private key to prove ownership of the service.
    /// The signature is stored in `cert_sig` field as base64 string.
    pub fn new_signed(
        kind: String,
        address: SocketAddr,
        version: u32,
        cert_pem: String,
        node_kp: &libp2p::identity::ed25519::Keypair
    ) -> Self {
        
        let sig = node_kp.sign(cert_pem.as_bytes());
        let sig_b64 = BASE64_STANDARD.encode(sig);

        ServiceInfo {
            kind,
            address,
            version,
            cert_pem: Some(cert_pem),
            cert_sig: Some(sig_b64)
        }
    }
    
    /// Verify the signature of the certificate using the provided public key.
    /// Returns true if the signature is valid, false otherwise.
    pub fn verify_signature(&self, peer_pk: &PublicKey) -> bool {
        
        // We can only verify if we have both cert and signature
        match (&self.cert_pem, &self.cert_sig) {
            (Some(pem), Some(sig_b64)) => {
                
                // Decode the base64 signature
                if let Ok(sig) = BASE64_STANDARD.decode(sig_b64) {
                    // Verify the signature
                    return peer_pk.verify(pem.as_bytes(), &sig);
                }
                false
            }
            
            _ => false, // No cert or signature to verify
        }
    }
    
    /// Returns the kind of the service.
    pub fn kind(&self) -> &str {
        &self.kind
    }
    
    /// Returns the address of the service.
    pub fn address(&self) -> &SocketAddr {
        &self.address
    }
    
    /// Returns the version of the service.
    pub fn version(&self) -> u32 {
        self.version
    }
    
    /// Returns the PEM string of TLS certificate used by the service.
    pub fn cert_pem(&self) -> Option<String> {
        self.cert_pem.clone()
    }
    
    /// Returns the signature of the certificate by the node's private key.
    pub fn cert_sig(&self) -> Option<String> {
        self.cert_sig.clone()
    }
}


/// Our ServiceInfo NetworkBehaviour relies on https://docs.rs/libp2p/latest/libp2p/request_response/cbor/type.Behaviour.html to perform serialization in binary format
pub type ServicesInfoBehaviour = RequestResponseBehaviour<ServicesInfoRequest, ServicesResponse>;

/// Definition of an inbound request or response for service-protocol
pub type ServicesInfoMessage = Message<ServicesInfoRequest, ServicesResponse>;

/// Type alias for ServicesInfo protocol events
pub type ServicesEvent  = ReqRespEvent<ServicesInfoRequest, ServicesResponse>;


impl From<ServicesEvent> for StryiEvent {
    fn from(e: ServicesEvent) -> Self { StryiEvent::Services(e) }
}
