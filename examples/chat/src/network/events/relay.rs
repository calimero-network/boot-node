use libp2p::relay;
use owo_colors::OwoColorize;
use tracing::{debug, error};

use super::{EventHandler, EventLoop, RelayReservationState};

impl EventHandler<relay::client::Event> for EventLoop {
    async fn handle(&mut self, event: relay::client::Event) {
        debug!("{}: {:?}", "relay".yellow(), event);

        match event {
            relay::client::Event::ReservationReqAccepted { relay_peer_id, .. } => {
                if let Some(entry) = self.relays.get_mut(&relay_peer_id) {
                    entry.reservation_state = RelayReservationState::Acquired;

                    for (peer_id, entry) in self.rendezvous.iter() {
                        if entry.identify_state.is_exchanged() {
                            if let Err(err) = self.swarm.behaviour_mut().rendezvous.register(
                                self.rendezvous_namespace.clone(),
                                *peer_id,
                                None,
                            ) {
                                error!(%err, "Failed to register at rendezvous");
                            }
                            debug!(
                                "Registered at rendezvous node {} on the namespace {}",
                                *peer_id, self.rendezvous_namespace
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
