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
                if let Err(err) = self
                    .network_state
                    .update_rendezvous_cookie(&rendezvous_node, cookie)
                {
                    error!(%err, "Failed to update peer rendezvous cookie");
                    return;
                };

                for registration in registrations {
                    if registration.record.peer_id() == *self.swarm.local_peer_id() {
                        continue;
                    }

                    let peer = registration.record.peer_id();
                    debug!(%peer, "Discovered peer via rendezvous");
                    if self.swarm.is_connected(&registration.record.peer_id()) {
                        continue;
                    };

                    for address in registration.record.addresses() {
                        debug!(%peer, %address, "Dialing peer discovered via rendezvous");
                        if let Err(err) = self.swarm.dial(address.clone()) {
                            error!("Failed to dial peer: {:?}", err);
                        }
                    }
                }
            }
            rendezvous::client::Event::Registered {
                rendezvous_node, ..
            } => {
                if let Some(peer_info) = self.network_state.get_peer_info(&rendezvous_node) {
                    if peer_info.rendezvous_cookie().is_none() {
                        debug!(%rendezvous_node, "Discovering peers via rendezvous after registration");
                        if let Err(err) = self.perform_rendezvous_discovery(&rendezvous_node) {
                            error!(%err, "Failed to run rendezvous discovery after registration");
                        }
                    }
                }
            }
            rendezvous::client::Event::DiscoverFailed {
                rendezvous_node,
                namespace,
                error,
            } => {
                error!(?rendezvous_node, ?namespace, error_code=?error, "Rendezvous discovery failed");
            }
            rendezvous::client::Event::RegisterFailed {
                rendezvous_node,
                namespace,
                error,
            } => {
                error!(?rendezvous_node, ?namespace, error_code=?error, "Rendezvous registration failed");
            }
            _ => {}
        }
    }
}
