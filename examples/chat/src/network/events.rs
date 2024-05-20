use tracing::error;

use super::*;

mod dcutr;
mod gossipsub;
mod identify;
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
                BehaviourEvent::Identify(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Gossipsub(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Mdns(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Rendezvous(event) => {
                    events::EventHandler::handle(self, event).await
                }
                BehaviourEvent::Relay(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Ping(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Dcutr(event) => events::EventHandler::handle(self, event).await,
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
                debug!(%peer_id, ?endpoint, "Connection established");
                match endpoint {
                    libp2p::core::ConnectedPoint::Dialer { .. } => {
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
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                debug!(%error, ?peer_id, "Outgoing connection error");
                if let Some(peer_id) = peer_id {
                    if let Some(sender) = self.pending_dial.remove(&peer_id) {
                        let _ = sender.send(Err(eyre::eyre!(error)));
                    }
                }
            }
            SwarmEvent::IncomingConnectionError { error, .. } => {
                debug!(%error, "Incoming connection error")
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
            SwarmEvent::ListenerError { error, .. } => debug!(%error, "Listener error"),
            SwarmEvent::NewExternalAddrCandidate { address } => {
                debug!("New external address candidate: {}", address)
            }
            SwarmEvent::ExternalAddrConfirmed { address } => {
                debug!("External address confirmed: {}", address);
            }
            SwarmEvent::ExternalAddrExpired { address } => {
                debug!("External address expired: {}", address)
            }
            SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
                debug!("New external address of peer: {} {}", peer_id, address)
            }
            _ => {}
        }
    }

    pub(super) async fn handle_rendezvous_discover(&mut self) {
        for (peer_id, entry) in self.rendezvous.iter() {
            if entry.indetify_state.is_exchanged() {
                self.swarm.behaviour_mut().rendezvous.discover(
                    Some(self.rendezvous_namespace.clone()),
                    entry.cookie.clone(),
                    None,
                    *peer_id,
                );
            }
        }
    }

    pub(super) async fn handle_rendezvous_dial(&mut self) {
        for (peer_id, entry) in self.rendezvous.iter() {
            if self.swarm.is_connected(peer_id) {
                continue;
            };
            if let Err(err) = self.swarm.dial(entry.address.clone()) {
                error!("Failed to dial rendezvous peer: {:?}", err);
            };
        }
    }

    pub(super) async fn handle_relays_dial(&mut self) {
        for (peer_id, entry) in self.relays.iter() {
            if self.swarm.is_connected(peer_id) {
                continue;
            };
            if let Err(err) = self.swarm.dial(entry.address.clone()) {
                error!("Failed to dial relay peer: {:?}", err);
            };
        }
    }
}
