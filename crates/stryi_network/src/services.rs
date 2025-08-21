//! This file defines our RequestResponse-based custom behaviour for obtaining up-to-date information about the peer's services
use std::net::SocketAddr;
use serde::{Deserialize, Serialize};
use libp2p::request_response::cbor::{Behaviour as RequestResponseBehaviour};
use libp2p::request_response::{Message, Event as ReqRespEvent};
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
    pub(crate) kind : String,

    /// Address(ip:port) in human-readable format e.g. 37.73.37.73:12240
    address : SocketAddr,

    /// Version of service to define if service is compatible with this node version.
    version: u32,
}



impl ServiceInfo {
    /// Construct a new ServiceInfo
    pub fn new(
        kind: String,
        address: SocketAddr,
        version: u32,
    ) -> Self {
        ServiceInfo {
            kind,
            address,
            version,
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
