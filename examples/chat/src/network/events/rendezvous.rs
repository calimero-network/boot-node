use libp2p::rendezvous;
use owo_colors::OwoColorize;
use tracing::{debug, error};

use super::{EventHandler, EventLoop};

impl EventHandler<rendezvous::client::Event> for EventLoop {
    async fn handle(&mut self, event: rendezvous::client::Event) {
        debug!("{}: {:?}", "rendezvous".yellow(), event);

        match event {
            rendezvous::client::Event::Discovered {
                rendezvous_node,
                registrations,
                cookie,
            } => {
                if let Some(entry) = self.rendezvous.get_mut(&rendezvous_node) {
                    entry.cookie = Some(cookie);
                }

                for registration in registrations {
                    if registration.record.peer_id() == *self.swarm.local_peer_id() {
                        continue;
                    }

                    if self.swarm.is_connected(&registration.record.peer_id()) {
                        continue;
                    };

                    for address in registration.record.addresses() {
                        let peer = registration.record.peer_id();
                        debug!(%peer, %address, "Discovered peer via rendezvous");

                        if let Err(err) = self.swarm.dial(address.clone()) {
                            error!("Failed to dial peer: {:?}", err);
                        }
                    }
                }
            }
            rendezvous::client::Event::Registered {
                rendezvous_node, ..
            } => {
                if let Some(entry) = self.rendezvous.get(&rendezvous_node) {
                    if entry.cookie.is_none() {
                        self.swarm.behaviour_mut().rendezvous.discover(
                            Some(self.rendezvous_namespace.clone()),
                            entry.cookie.clone(),
                            None,
                            rendezvous_node,
                        );
                    }
                }
            }
            rendezvous::client::Event::Expired { peer } => {
                let local_peer_id = *self.swarm.local_peer_id();
                if peer == local_peer_id {
                    if let Err(err) = self.swarm.behaviour_mut().rendezvous.register(
                        libp2p::rendezvous::Namespace::from_static("rendezvous"),
                        peer,
                        None,
                    ) {
                        error!(%err, "Failed to re-register at rendezvous");
                    };
                    debug!("Re-registered at rendezvous");
                }
            }
            _ => {}
        }
    }
}
