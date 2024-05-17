use libp2p::identify;
use owo_colors::OwoColorize;
use tracing::{debug, error};

use super::{types, EventHandler, EventLoop};

impl EventHandler<identify::Event> for EventLoop {
    async fn handle(&mut self, event: identify::Event) {
        debug!("{}: {:?}", "identify".yellow(), event);

        match event {
            identify::Event::Received {
                peer_id,
                info: identify::Info { observed_addr, .. },
            } => {
                if let Err(err) = self
                    .event_sender
                    .send(types::NetworkEvent::IdentifyReceived {
                        peer_id,
                        observed_addr,
                    })
                    .await
                {
                    error!("Failed to send message event: {:?}", err);
                }
            }
            identify::Event::Sent { peer_id } => {
                if let Err(err) = self
                    .event_sender
                    .send(types::NetworkEvent::IdentifySent { peer_id })
                    .await
                {
                    error!("Failed to send message event: {:?}", err);
                }
            }
            _ => {}
        }
    }
}
