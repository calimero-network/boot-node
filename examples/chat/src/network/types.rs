pub use libp2p::gossipsub::{Message, TopicHash};
pub use libp2p::identity::PeerId;

use super::stream;

#[derive(Debug)]
pub enum NetworkEvent {
    ListeningOn {
        address: libp2p::Multiaddr,
    },
    Subscribed {
        peer_id: PeerId,
        topic: TopicHash,
    },
    Message {
        message: Message,
    },
    StreamOpened {
        peer_id: PeerId,
        stream: stream::Stream,
    },
}
