use libp2p::relay;
use owo_colors::OwoColorize;
use tracing::{debug, error};

use super::{types, EventHandler, EventLoop};

impl EventHandler<relay::client::Event> for EventLoop {
    async fn handle(&mut self, event: relay::client::Event) {
        debug!("{}: {:?}", "relay_client".yellow(), event);

        match event {
            relay::client::Event::ReservationReqAccepted { .. } => {
                if let Err(err) = self
                    .event_sender
                    .send(types::NetworkEvent::RelayReservationAccepted)
                    .await
                {
                    error!("Failed to send message event: {:?}", err);
                }
            }
            _ => {}
        }
    }
}
