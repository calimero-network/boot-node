use libp2p::identify;
use owo_colors::OwoColorize;
use tracing::{debug, error};

use super::{EventHandler, EventLoop, RelayReservationState};

impl EventHandler<identify::Event> for EventLoop {
    async fn handle(&mut self, event: identify::Event) {
        debug!("{}: {:?}", "identify".yellow(), event);

        match event {
            identify::Event::Received { peer_id, .. } => {
                if let Some(entry) = self.relays.get_mut(&peer_id) {
                    entry.identify_state.received = true;

                    if entry.identify_state.is_exchanged()
                        && matches!(entry.reservation_state, RelayReservationState::Unknown)
                    {
                        if let Err(err) = self
                            .swarm
                            .listen_on(entry.address.clone().with(multiaddr::Protocol::P2pCircuit))
                        {
                            error!("Failed to listen on relay address: {:?}", err);
                        };
                        entry.reservation_state = RelayReservationState::Requested;
                    }
                }

                if let Some(entry) = self.rendezvous.get_mut(&peer_id) {
                    entry.identify_state.received = true;
                }
            }
            identify::Event::Sent { peer_id } => {
                if let Some(entry) = self.relays.get_mut(&peer_id) {
                    entry.identify_state.sent = true;

                    if entry.identify_state.is_exchanged()
                        && matches!(entry.reservation_state, RelayReservationState::Unknown)
                    {
                        if let Err(err) = self
                            .swarm
                            .listen_on(entry.address.clone().with(multiaddr::Protocol::P2pCircuit))
                        {
                            error!("Failed to listen on relay address: {:?}", err);
                        };
                        entry.reservation_state = RelayReservationState::Requested;
                    }
                }

                if let Some(entry) = self.rendezvous.get_mut(&peer_id) {
                    entry.identify_state.sent = true;
                }
            }
            _ => {}
        }
    }
}
