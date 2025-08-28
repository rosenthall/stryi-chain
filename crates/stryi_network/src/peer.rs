use std::collections::HashMap;
use std::time::SystemTime;
use libp2p::{Multiaddr, PeerId, identity::PublicKey};
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
    
    /// Public key of the peer (if known).
    pub public_key: Option<PublicKey>,
    
    /// Number of consecutive ping failures for this peer.
    /// This is used to detect unresponsive peers and potentially disconnect them.
    pub consecutive_ping_failures: usize,

}

impl PeerInfo {
    pub fn new_now(remote: Option<Multiaddr>) -> Self {
        Self {
            established_at: Some(SystemTime::now()),
            last_seen:      None,
            addresses:      remote.into_iter().collect(), // zero or one
            public_key:     None,
            services:       Vec::new(),
            consecutive_ping_failures: 0,
        }
    }

    pub fn mark_seen(&mut self) {
        self.last_seen = Some(SystemTime::now());
    }

    pub fn add_address(&mut self, addr: Multiaddr) {
        if !self.addresses.iter().any(|a| a == &addr) {
            self.addresses.push(addr);
        }
    }

    pub fn merge_addresses<I: IntoIterator<Item = Multiaddr>>(&mut self, addrs: I) {
        for a in addrs {
            self.add_address(a);
        }
    }

    pub fn set_services(&mut self, services: Vec<ServiceInfo>) {
        self.services = services;
        self.mark_seen();
    }

    pub fn ping_ok(&mut self) {
        self.consecutive_ping_failures = 0;
        self.mark_seen();
    }

    pub fn ping_fail(&mut self) -> usize {
        self.consecutive_ping_failures += 1;
        self.consecutive_ping_failures
    }
}




pub trait PeerMapExt {
    /// Update or insert a peer as connected, setting the established_at time if not already set.
    fn upsert_connected(&mut self, peer: PeerId, remote: Multiaddr);
    /// Update or insert a peer with identify information, merging listen addresses and observed address.
    fn upsert_identify(&mut self, peer: PeerId, listen_addrs: Vec<Multiaddr>, observed: Option<Multiaddr>, public_key: PublicKey,);
    /// Update or insert a peer with services information.
    fn upsert_services(&mut self, peer: PeerId, services: Vec<ServiceInfo>);
    
    /// Record a successful ping – returns the new consecutive-failure count (always 0).
    fn ping_success(&mut self, peer: PeerId) -> usize;

    /// Record a failed ping – returns the updated consecutive-failure count.
    fn ping_failure(&mut self, peer: PeerId) -> usize;
}

impl PeerMapExt for PeerMap {
    fn upsert_connected(&mut self, peer: PeerId, remote: Multiaddr) {
        self.entry(peer)
            .and_modify(|pi| {
                pi.add_address(remote.clone());
                pi.established_at.get_or_insert(SystemTime::now());
            })
            .or_insert_with(|| PeerInfo::new_now(Some(remote)));
    }

    fn upsert_identify(
        &mut self,
        peer: PeerId,
        listen_addrs: Vec<Multiaddr>,
        observed: Option<Multiaddr>,
        public_key: PublicKey,
    ) {
        let pi = self.entry(peer).or_insert_with(|| PeerInfo::new_now(None));

        // merge addresses
        pi.merge_addresses(listen_addrs);
        if let Some(obs) = observed {
            pi.add_address(obs);
        }

        // store key exactly once, detect mismatch
        match &pi.public_key {
            None => pi.public_key = Some(public_key),
            Some(pk) if pk != &public_key => {
                tracing::warn!(
                    "Peer {} presented a different public key (possible mis-configuration)",
                    peer
                );
            }
            _ => {}
        }

        pi.mark_seen();
    }

    fn upsert_services(&mut self, peer: PeerId, services: Vec<ServiceInfo>) {
        let pi = self.entry(peer).or_insert_with(|| PeerInfo::new_now(None));
        pi.set_services(services);
    }


    fn ping_success(&mut self, peer: PeerId) -> usize {
        let pi = self.entry(peer).or_insert_with(|| PeerInfo::new_now(None));
        pi.ping_ok();
        0
    }

    fn ping_failure(&mut self, peer: PeerId) -> usize {
        let pi = self.entry(peer).or_insert_with(|| PeerInfo::new_now(None));
        pi.ping_fail()
    }
}

