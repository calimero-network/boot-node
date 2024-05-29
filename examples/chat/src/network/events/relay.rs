use libp2p::relay;
use owo_colors::OwoColorize;
use tracing::{debug, error};

use super::{discovery, EventHandler, EventLoop};

impl EventHandler<relay::client::Event> for EventLoop {
    async fn handle(&mut self, event: relay::client::Event) {
        debug!("{}: {:?}", "relay".yellow(), event);

        match event {
            relay::client::Event::ReservationReqAccepted { relay_peer_id, .. } => {
                if let Err(err) = self.network_state.update_relay_reservation_status(
                    &relay_peer_id,
                    discovery::model::RelayReservationStatus::Accepted,
                ) {
                    error!(%err, "Failed to update peer relay reservation status");
                    return;
                }
            }
            _ => {}
        }
    }
}
