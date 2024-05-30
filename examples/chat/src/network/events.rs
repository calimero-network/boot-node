use tracing::error;

use super::*;

mod dcutr;
mod gossipsub;
mod identify;
mod kad;
mod mdns;
mod ping;
mod relay;
mod rendezvous;

pub trait EventHandler<E> {
    async fn handle(&mut self, event: E);
}

impl EventLoop {
    pub(super) async fn handle_swarm_event(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(event) => match event {
                BehaviourEvent::Dcutr(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Gossipsub(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Identify(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Kad(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Mdns(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Ping(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Relay(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Rendezvous(event) => {
                    events::EventHandler::handle(self, event).await
                }
            },
            SwarmEvent::NewListenAddr {
                listener_id,
                address,
            } => {
                let local_peer_id = *self.swarm.local_peer_id();
                let address = match address.with_p2p(local_peer_id) {
                    Ok(address) => address,
                    Err(address) => {
                        warn!(
                            "Failed to sanitize listen address with p2p proto, address: {:?}, p2p proto: {:?}",
                            address, local_peer_id
                        );
                        address
                    }
                };
                if let Err(err) = self
                    .event_sender
                    .send(types::NetworkEvent::ListeningOn {
                        listener_id,
                        address,
                    })
                    .await
                {
                    error!("Failed to send listening on event: {:?}", err);
                };
            }
            SwarmEvent::IncomingConnection { .. } => {}
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                info!(%peer_id, ?endpoint, "Connection established");
                match endpoint {
                    libp2p::core::ConnectedPoint::Dialer { .. } => {
                        self.discovery_state
                            .add_peer_addr(peer_id, endpoint.get_remote_address());

                        if let Some(sender) = self.pending_dial.remove(&peer_id) {
                            let _ = sender.send(Ok(Some(())));
                        }
                    }
                    _ => {}
                }
            }
            SwarmEvent::ConnectionClosed {
                peer_id,
                connection_id,
                endpoint,
                num_established,
                cause,
            } => {
                debug!(
                    "Connection closed: {} {:?} {:?} {} {:?}",
                    peer_id, connection_id, endpoint, num_established, cause
                );
                if !self.swarm.is_connected(&peer_id)
                    && !self.discovery_state.is_peer_of_interest(&peer_id)
                {
                    self.discovery_state.remove_peer(&peer_id);
                }
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                debug!(%error, ?peer_id, "Outgoing connection error");
                if let Some(peer_id) = peer_id {
                    if let Some(sender) = self.pending_dial.remove(&peer_id) {
                        let _ = sender.send(Err(eyre::eyre!(error)));
                    }
                }
            }
            SwarmEvent::IncomingConnectionError {
                send_back_addr,
                error,
                ..
            } => {
                debug!(%error, %send_back_addr, "Incoming connection error")
            }
            SwarmEvent::Dialing {
                peer_id: Some(peer_id),
                ..
            } => trace!("Dialing peer: {}", peer_id),
            SwarmEvent::ExpiredListenAddr { address, .. } => {
                debug!("Expired listen address: {}", address)
            }
            SwarmEvent::ListenerClosed {
                addresses, reason, ..
            } => {
                debug!("Listener closed: {:?} {:?}", addresses, reason.err())
            }
            SwarmEvent::ListenerError { error, .. } => info!(%error, "Listener error"),
            SwarmEvent::NewExternalAddrCandidate { address } => {
                debug!("New external address candidate: {}", address)
            }
            SwarmEvent::ExternalAddrConfirmed { address } => {
                debug!("External address confirmed: {}", address);
                self.discovery_state.set_pending_addr_changes();

                if let Err(err) = self.broadcast_rendezvous_registrations() {
                    error!(%err, "Failed to handle rendezvous register");
                };
            }
            SwarmEvent::ExternalAddrExpired { address } => {
                debug!("External address expired: {}", address);
                self.discovery_state.set_pending_addr_changes();

                if let Err(err) = self.broadcast_rendezvous_registrations() {
                    error!(%err, "Failed to handle rendezvous register");
                };
            }
            SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
                debug!("New external address of peer: {} {}", peer_id, address)
            }
            _ => {}
        }
    }
}
