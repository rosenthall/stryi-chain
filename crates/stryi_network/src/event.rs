use std::time::SystemTime;
use crate::mempool::{MempoolRequest, MempoolResponse};
use crate::services::{ServicesInfoRequest, ServicesResponse};
use crate::{BroadcastBlock, NetworkEvent, PeerInfo, StryiBehaviour, StryiEvent, StryiNetworkError, StryiNetworkManager};
use bincode::config::standard;
use bincode::serde::decode_from_slice;
use libp2p::request_response::Event as ReqRespEvent;
use libp2p::request_response::{InboundRequestId, ResponseChannel};
use libp2p::swarm::SwarmEvent;
use libp2p::{Swarm, gossipsub, request_response};
use libp2p::ping::{Event as PingEvent};
use stryi_core::transactions::Transaction;
use tracing::{debug, error, info, trace, warn};

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
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                info!("Connected to {}", peer_id);
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                info!("Disconnected from {}", peer_id);
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
                    }

                    // --- Gossipsub ---
                    StryiEvent::Gossipsub(gossipsub_event) => {
                        self.handle_gossipsub_event(gossipsub_event).await.ok();
                    }

                    // ---- Ping ----
                    StryiEvent::Ping(PingEvent { peer, result, .. }) => {
                        match result {

                            // If the ping was successful, update the peer info
                            Ok(rtt) => {
                                let mut peers = self.connected_peers.write().await;
                                let info = peers.entry(peer).or_insert_with(|| PeerInfo {
                                    established_at: Some(SystemTime::now()),
                                    last_seen: None,
                                    addresses: Vec::new(),
                                    grpc_sync_server_address: None,
                                    consecutive_ping_failures: 0, // any new correct ping resets the failure count
                                });
                                info.last_seen = Some(SystemTime::now());
                                info.consecutive_ping_failures = 0;
                                debug!("ping {} rtt = {:?}", peer, rtt);
                            }

                            // If the ping failed, log the error and check if we need to disconnect the peer (in case of too many failures in a row)
                            Err(err) => {
                                const MAX_PING_FAILURES: usize = 10;
                                warn!("ping {} failed: {}", peer, err);
                                let mut drop_now = false;
                                {
                                    let mut peers = self.connected_peers.write().await;
                                    if let Some(info) = peers.get_mut(&peer) {
                                        info.consecutive_ping_failures += 1;
                                        drop_now = info.consecutive_ping_failures >= MAX_PING_FAILURES;
                                    }
                                }
                                if drop_now {
                                    warn!("Disconnecting unresponsive peer {}", peer);
                                    if let Err(e) = swarm.disconnect_peer_id(peer) {
                                        error!("Disconnect error: {:?}", e);
                                    }
                                }
                            }
                        }
                    }

                    // --- Identify ---

                    // TODO: Setup Identify events handling

                    // -- temporal stubs --
                    StryiEvent::Identify(e) => debug!("Identify event: {:?}", e),
                    StryiEvent::RzvServer(e) => debug!("Rendezvous Server event: {:?}", e),
                    StryiEvent::RzvClient(e) => debug!("Rendezvous Client event: {:?}", e),

                    // Fallback
                    other => {
                        debug!("Got unhandled custom event: {:?}", other);
                    }
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
                    TRANSACTIONS_TOPIC_NAME => {
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
                    BLOCKS_TOPIC_NAME => {
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

    /// Handles an inbound Services-Info request.
    async fn handle_services_info_request(
        &self,
        behaviour: &mut StryiBehaviour,
        _request_id: InboundRequestId,
        request: ServicesInfoRequest,
        channel: ResponseChannel<ServicesResponse>,
    ) -> Result<(), StryiNetworkError> {
        trace!("ServicesInfo request: {:?}", request);

        match request {
            ServicesInfoRequest::ListServices => {
                // Build the response from the shared services_info.
                let services = self.services_info.read().await.clone();
                let response = ServicesResponse { services };

                // Respond via the behaviour that's actually attached to the Swarm.
                // NOTE: adjust the field/method name if your StryiBehaviour exposes a different handle.
                behaviour
                    .services_info
                    .send_response(channel, response)
                    .map_err(|_| {
                        StryiNetworkError::other("failed to send ServicesInfo response")
                    })?;
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
