use tracing::error;

use super::*;

mod dcutr;
mod gossipsub;
mod identify;
mod ping;
mod relay_client;

pub trait EventHandler<E> {
    async fn handle(&mut self, event: E);
}

impl EventLoop {
    pub(super) async fn handle_swarm_event(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::Behaviour(event) => match event {
                BehaviourEvent::Identify(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Gossipsub(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::RelayClient(event) => {
                    events::EventHandler::handle(self, event).await
                }
                BehaviourEvent::Ping(event) => events::EventHandler::handle(self, event).await,
                BehaviourEvent::Dcutr(event) => events::EventHandler::handle(self, event).await,
            },
            SwarmEvent::NewListenAddr {
                listener_id,
                address,
            } => {
                let local_peer_id = *self.swarm.local_peer_id();
                if let Err(err) = self
                    .event_sender
                    .send(types::NetworkEvent::ListeningOn {
                        listener_id,
                        address: address.with(multiaddr::Protocol::P2p(local_peer_id)),
                    })
                    .await
                {
                    error!("Failed to send listening on event: {:?}", err);
                }
            }
            SwarmEvent::IncomingConnection { .. } => {}
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                debug!(%peer_id, ?endpoint, "Connection established");
                match endpoint {
                    libp2p::core::ConnectedPoint::Dialer { address, .. } => {
                        let addr_meta = match MultiaddrMeta::try_from(&address) {
                            Ok(meta) => meta,
                            Err(e) => {
                                error!(%e, "Failed to parse dialer address meta for established connection");
                                return;
                            }
                        };

                        if addr_meta.is_relayed() {
                            debug!("Connection established via relay");
                        }

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
                trace!("Expired listen address: {}", address)
            }
            SwarmEvent::ListenerClosed {
                addresses, reason, ..
            } => trace!("Listener closed: {:?} {:?}", addresses, reason.err()),
            SwarmEvent::ListenerError { error, .. } => trace!(%error, "Listener error"),
            SwarmEvent::NewExternalAddrCandidate { address } => {
                trace!("New external address candidate: {}", address)
            }
            SwarmEvent::ExternalAddrConfirmed { address } => {
                trace!("External address confirmed: {}", address)
            }
            SwarmEvent::ExternalAddrExpired { address } => {
                trace!("External address expired: {}", address)
            }
            unhandled => warn!("Unhandled event: {:?}", unhandled),
        }
    }
}
