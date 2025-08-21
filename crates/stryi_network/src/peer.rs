use std::collections::HashMap;
use std::time::SystemTime;
use libp2p::{Multiaddr, PeerId};
use crate::ServiceInfo;

type PeerMap = HashMap<PeerId, PeerInfo>;

#[derive(Clone, Debug)]
pub struct PeerInfo {
    /// Time the connection was established, if known.
    pub established_at: Option<SystemTime>,
    
    /// Most recent time the peer was seen or updated.
    pub last_seen: Option<SystemTime>,
    
    /// All known addresses for this peer. TODO: Is storing more than 1 address of single peer is necessary for design?
    pub addresses: Vec<Multiaddr>,

    /// Cached answer of `ServicesInfoRequest` for this peer.
    pub services:  Vec<ServiceInfo>,
    
    /// Number of consecutive ping failures for this peer.
    /// This is used to detect unresponsive peers and potentially disconnect them.
    pub consecutive_ping_failures: usize,

}
