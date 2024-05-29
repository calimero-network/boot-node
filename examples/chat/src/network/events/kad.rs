use libp2p::kad;
use owo_colors::OwoColorize;
use tracing::debug;

use super::{EventHandler, EventLoop};

impl EventHandler<kad::Event> for EventLoop {
    async fn handle(&mut self, event: kad::Event) {
        debug!("{}: {:?}", "kad".yellow(), event);
    }
}
