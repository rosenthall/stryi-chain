use crate::mempool::{MempoolRequest, MempoolResponse};
use crate::services::{ServicesInfoRequest, ServicesResponse};
use crate::{manager, BroadcastBlock, NetworkEvent, StryiBehaviour, StryiEvent, StryiNetworkError, StryiNetworkManager};
use bincode::config::standard;
use bincode::serde::decode_from_slice;
use libp2p::request_response::{Event as ReqRespEvent, Message};
use libp2p::identify::{Event as IdentifyEvent, Info as IdentifyInfo};
use libp2p::request_response::{InboundRequestId, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use libp2p::{Swarm, gossipsub, request_response, Multiaddr};
use libp2p::core::ConnectedPoint;
use libp2p::ping::{Event as PingEvent};
use stryi_core::transactions::Transaction;
use tracing::{debug, error, info, trace, warn};
use crate::peer::PeerMapExt;

const MAX_PING_FAILURES: usize = 10;

fn remote_addr_from_endpoint(endpoint: &ConnectedPoint) -> Multiaddr {
    match endpoint {
        ConnectedPoint::Dialer { address, .. } => address.clone(),
        ConnectedPoint::Listener { send_back_addr, .. } => send_back_addr.clone(),
    }
}

impl StryiNetworkManager {
    /// Process a single event from the swarm, handling it according to its type.
    /// This function is called by the main event loop of the network manager.
    pub(crate) async fn process_event(
        &self,
        swarm: &mut Swarm<StryiBehaviour>,
        event: SwarmEvent<StryiEvent>,
    ) {
        // Log the event for debugging purposes
        trace!("Processing event: {:?}", event);

        match event {
            // --- Utils ---
            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {}", address);
            }

            SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                let remote = remote_addr_from_endpoint(&endpoint);
                {
                    let mut peers = self.connected_peers.write().await;
                    peers.upsert_connected(peer_id, remote);
                }
            }


            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                info!("Disconnected from {}", peer_id);
                self.connected_peers.write().await.remove(&peer_id);
            }


            // -- Stryichain's custom behaviour events --
            SwarmEvent::Behaviour(behaviour_event) => {
                match behaviour_event {
                    // --- Mempool ---
                    StryiEvent::Mempool(ev) => {
                        if let ReqRespEvent::Message {
                            message:
                                request_response::Message::Request {
                                    request_id,
                                    request,
                                    channel,
                                },
                            ..
                        } = ev
                        {
                            let behaviour_ref = swarm.behaviour_mut();
                            if let Err(e) = self
                                .handle_mempool_request(behaviour_ref, request_id, request, channel)
                                .await
                            {
                                error!("handle_mempool_request failed: {e:?}");
                            }
                        }
                    }

                    // --- Services Info ---
                    StryiEvent::Services(ev) => {
                        match ev {
                            // inbound request
                            ReqRespEvent::Message {
                                peer: _,
                                message:
                                request_response::Message::Request {
                                    request_id,
                                    request,
                                    channel,
                                },
                                ..
                            } => {

                                // Handle inbound request for services info
                                let behaviour_ref = swarm.behaviour_mut();
                                if let Err(e) = self
                                    .handle_services_info_request(
                                        behaviour_ref,
                                        request_id,
                                        request,
                                        channel,
                                    )
                                    .await
                                {
                                    error!("handle_services_info_request failed: {e:?}");
                                }
                            }

                            // Inbound response: cache peer's services
                            ReqRespEvent::Message {
                                peer,
                                message: Message::Response { response, .. },
                                ..
                            } => {
                                let ServicesResponse { services } = response;
                                {
                                    let mut peers = self.connected_peers.write().await;
                                    peers.upsert_services(peer, services);     // helper stores + timestamps
                                }
                                trace!("cached services for peer {}", peer);
                            }


                            // Channel failures (just diagnostics)
                            ReqRespEvent::OutboundFailure { peer, error, request_id, .. } => {
                                warn!("ServicesInfo outbound failure peer={} req_id={:?} err={:?}", peer, request_id, error);
                            }
                            ReqRespEvent::InboundFailure  { peer, error, request_id, .. } => {
                                warn!("ServicesInfo inbound failure peer={} req_id={:?}  err={:?}", peer, request_id, error);
                            }

                            _ => {
                                // Handle other events if needed
                                debug!("Unhandled ServicesInfo event: {:?}", ev);
                            }
                        }
                    }

                    // --- Gossipsub ---
                    StryiEvent::Gossipsub(gossipsub_event) => {
                        self.handle_gossipsub_event(gossipsub_event).await.ok();
                    }
                    StryiEvent::Ping(PingEvent { peer, result, .. }) => {
                        match result {

                            // Success path: mark last_seen, reset failure counter
                            Ok(rtt) => {
                                {
                                    let mut peers = self.connected_peers.write().await;
                                    peers.ping_success(peer);
                                }
                                debug!("ping {} rtt = {:?}", peer, rtt);
                            }

                            // Failure path: increment failure counter and optionally disconnect
                            Err(err) => {
                                warn!("ping {} failed: {}", peer, err);

                                // update counter under lock, grab the new value
                                let failures = {
                                    let mut peers = self.connected_peers.write().await;
                                    peers.ping_failure(peer)                // helper returns the count
                                };

                                debug!("ping {} consecutive failures = {}", peer, failures);

                                if failures >= MAX_PING_FAILURES {
                                    warn!("Disconnecting unresponsive peer {} (failures >= {})", peer, MAX_PING_FAILURES);
                                    if let Err(e) = swarm.disconnect_peer_id(peer) {
                                        error!("Disconnect error for {}: {:?}", peer, e);
                                    }
                                }
                            }
                        }
                    }

                    // --- Identify ---



                    StryiEvent::Identify(id_ev) => {
                        match id_ev {
                            IdentifyEvent::Received { peer_id, info, .. } => {
                                // Cache listen_addrs and observed_addr
                                let IdentifyInfo { listen_addrs, observed_addr, public_key, .. } = info;

                                {
                                    let mut peers = self.connected_peers.write().await;
                                    peers.upsert_identify(
                                        peer_id,
                                        listen_addrs,
                                        Some(observed_addr),
                                        public_key.clone(),
                                    );
                                }


                                // Send a single ServicesInfo request now (after Identify)
                                let req_id = swarm
                                    .behaviour_mut()
                                    .services_info
                                    .send_request(&peer_id, ServicesInfoRequest::ListServices);
                                info!("Sent ServicesInfo request to {} (req_id={:?}) after Identify", peer_id, req_id);
                            }

                            IdentifyEvent::Sent { peer_id, ..} => {
                                debug!("Identify sent to {}", peer_id);
                            }

                            IdentifyEvent::Error { peer_id, error , ..} => {
                                warn!("Identify error with {}: {}", peer_id, error);
                            }

                            other => {
                                debug!("Identify event: {:?}", other);
                            }
                        }
                    }

                    // -- temporal stubs --
                    StryiEvent::RzvServer(e) => debug!("Rendezvous Server event: {:?}", e),
                    StryiEvent::RzvClient(e) => debug!("Rendezvous Client event: {:?}", e),

                }
            }

            // todo: process other events, such as dialing, gossipsub, rendezvous(server), identify, and ping. For now just log and keep looping
            _ => debug!("Got event: {:?}", event),
        }
    }

    // handles gossipsub requests
    async fn handle_gossipsub_event(
        &self,
        event: gossipsub::Event,
    ) -> Result<(), StryiNetworkError> {
        trace!("Got gossipsub event {:?}", event);

        match event {
            gossipsub::Event::Message {
                propagation_source,
                message_id,
                message,
            } => {
                trace!(
                    "Got gossipsub message! Propagation source : {}, id : {}",
                    propagation_source,
                    &message_id.to_string()
                );

                // Check the topics name and define how to proceed message correspondingly
                match message.topic.as_str() {
                    // Try to process everything from transactions topic as a transaction
                    manager::TRANSACTIONS_TOPIC_NAME => {
                        let tx: Transaction = decode_from_slice(&message.data, standard())
                            .map_err(StryiNetworkError::DecodeGossipsubMessageError)?
                            .0;
                        debug!("Received transaction {} in gossipsub", &tx.data.hash());

                        // try to generate and send `NewTransaction` event
                        self.event_tx
                            .send(NetworkEvent::NewTransaction(tx))
                            .map_err(StryiNetworkError::CannotSendEvent)?;
                    }

                    // and from blocks topic as a block
                    manager::BLOCKS_TOPIC_NAME => {
                        let broadcast_block: BroadcastBlock =
                            decode_from_slice(&message.data, standard())
                                .map_err(StryiNetworkError::DecodeGossipsubMessageError)?
                                .0;
                        debug!(
                            "Received block {} in gossipsub",
                            &broadcast_block.block.block_hash()
                        );

                        // Try generate and send `NewBlock` event
                        self.event_tx
                            .send(NetworkEvent::NewBlock(broadcast_block))
                            .map_err(StryiNetworkError::CannotSendEvent)?;
                    }

                    // We don't care about all another topics
                    _ => {}
                };

                Ok(())
            }
            gossipsub::Event::Subscribed { .. } => Ok(()),
            gossipsub::Event::Unsubscribed { .. } => Ok(()),

            // We don't care about these two I guess
            gossipsub::Event::GossipsubNotSupported { .. } => Ok(()),
            gossipsub::Event::SlowPeer { .. } => Ok(()),
        }
    }


    /// Handles an inbound Services-Info request (we are responder).
    async fn handle_services_info_request(
        &self,
        behaviour: &mut StryiBehaviour,
        _request_id: InboundRequestId,
        request: ServicesInfoRequest,
        channel: ResponseChannel<ServicesResponse>,
    ) -> Result<(), StryiNetworkError> {
        trace!("ServicesInfo request: {:?}", request);


        match request {
            // If the request is to get the list of services, we will respond with the current services info.
            ServicesInfoRequest::ListServices => {
                // Build response from local services' registry.
                let services = self.services_info.read().await.clone();
                let response = ServicesResponse { services };

                // Send response back to the requester.
                behaviour
                    .services_info
                    .send_response(channel, response)
                    .map_err(|_| StryiNetworkError::other("failed to send ServicesInfo response"))?;
            }
        }



        Ok(())
    }


    // handles mempool requests
    pub(crate) async fn handle_mempool_request(
        &self,
        behaviour: &mut StryiBehaviour,
        _request_id: InboundRequestId,
        request: MempoolRequest,
        channel: ResponseChannel<MempoolResponse>,
    ) -> Result<(), StryiNetworkError> {
        trace!("Mempool request: {:?}", request);

        match request {
            MempoolRequest::GetState => {
                // Ask the mempool for its sync snapshot.
                let state = self
                    .mempool
                    .read()
                    .await
                    .get_sync_state()
                    .await
                    .map_err(StryiNetworkError::MempoolError)?;

                let response = MempoolResponse::State(state);

                behaviour
                    .mempool_sync
                    .send_response(channel, response)
                    .map_err(|_| StryiNetworkError::other("failed to send Mempool response"))?;
            }
        }

        Ok(())
    }
}
