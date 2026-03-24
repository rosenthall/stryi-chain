use crate::services::SignedServiceRecord;
use libp2p::{Multiaddr, PeerId, identity::PublicKey};
use std::collections::HashMap;
use std::time::SystemTime;

type PeerMap = HashMap<PeerId, PeerInfo>;

/// Per-peer state tracked by the network layer.
#[derive(Clone, Debug, Default)]
pub struct PeerInfo {
    /// Time the connection was established, if known.
    pub established_at: Option<SystemTime>,

    /// Most recent time the peer was seen or updated.
    pub last_seen: Option<SystemTime>,

    /// All known addresses for this peer.  
    // NOTE: Is storing >1 address for a single peer necessary for design?
    pub addresses: Vec<Multiaddr>,

    /// Cached signed service advertisements received from this peer
    /// ready for retransmission.
    pub services: Vec<SignedServiceRecord>,

    /// Public key of the peer (if known).
    pub public_key: Option<PublicKey>,

    /// Number of consecutive ping failures for this peer.  
    /// This is used to detect unresponsive peers and potentially disconnect them.
    pub consecutive_ping_failures: usize,
}

impl PeerInfo {
    /// Constructor used when a new connection is observed.
    pub fn new_now(remote: Option<Multiaddr>) -> Self {
        Self {
            established_at: Some(SystemTime::now()),
            last_seen: None,
            addresses: remote.into_iter().collect(),
            public_key: None,
            services: Vec::new(),
            consecutive_ping_failures: 0,
        }
    }

    /// Update `last_seen` timestamp.
    pub fn mark_seen(&mut self) {
        self.last_seen = Some(SystemTime::now());
    }

    /// Add a single address if it is not already known.
    pub fn add_address(&mut self, addr: Multiaddr) {
        if !self.addresses.iter().any(|a| a == &addr) {
            self.addresses.push(addr);
        }
    }

    /// Merge multiple addresses.
    pub fn merge_addresses<I: IntoIterator<Item = Multiaddr>>(&mut self, addrs: I) {
        for a in addrs {
            self.add_address(a);
        }
    }

    /// Replace the signed service list, update `last_seen`.
    pub fn set_signed_services(&mut self, signed: Vec<SignedServiceRecord>) {
        self.services = signed;
        self.mark_seen();
    }

    /// Reset ping-failure counter.
    pub fn ping_ok(&mut self) {
        self.consecutive_ping_failures = 0;
        self.mark_seen();
    }

    /// Increment ping-failure counter.
    pub fn ping_fail(&mut self) -> usize {
        self.consecutive_ping_failures += 1;
        self.consecutive_ping_failures
    }
}

/// Helper methods operating on `PeerMap`.
pub trait PeerMapExt {
    /// Update or insert a peer as connected, setting the established_at time if not already set.
    fn upsert_connected(&mut self, peer: PeerId, remote: Multiaddr);

    /// Update or insert a peer with identify information, merging listen addresses and observed address.
    fn upsert_identify(
        &mut self,
        peer: PeerId,
        listen_addrs: Vec<Multiaddr>,
        observed: Option<Multiaddr>,
        public_key: PublicKey,
    );

    /// Store signed service announcements as-is (single source of truth).
    fn set_signed_services(&mut self, peer: PeerId, signed: Vec<SignedServiceRecord>);

    /// Record a successful ping - returns the new consecutive-failure count (always 0).
    fn ping_success(&mut self, peer: PeerId) -> usize;

    /// Record a failed ping - returns the updated consecutive-failure count.
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

        // merge listen + observed addresses
        pi.merge_addresses(listen_addrs);
        if let Some(obs) = observed {
            pi.add_address(obs);
        }

        // store key exactly once, detect mismatch
        match &pi.public_key {
            None => pi.public_key = Some(public_key),
            Some(pk) if pk != &public_key => {
                tracing::warn!(
                    "Peer {} presented a different public key (possible misconfiguration)",
                    peer
                );
            }
            _ => {}
        }

        pi.mark_seen();
    }

    fn set_signed_services(&mut self, peer: PeerId, signed: Vec<SignedServiceRecord>) {
        const MAX_SERVICES: usize = 32; // simple abuse-resistance guard
        let bounded = signed.into_iter().take(MAX_SERVICES).collect();

        let pi = self.entry(peer).or_insert_with(|| PeerInfo::new_now(None));
        pi.set_signed_services(bounded);
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
