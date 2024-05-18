use libp2p::core::transport;
pub use libp2p::gossipsub::{Message, MessageId, TopicHash};
pub use libp2p::identity::PeerId;

#[derive(Debug)]
pub enum NetworkEvent {
    ListeningOn {
        listener_id: transport::ListenerId,
        address: libp2p::Multiaddr,
    },
    Subscribed {
        peer_id: PeerId,
        topic: TopicHash,
    },
    Message {
        id: MessageId,
        message: Message,
    },
    IdentifySent {
        peer_id: PeerId,
    },
    IdentifyReceived {
        peer_id: PeerId,
        observed_addr: libp2p::Multiaddr,
    },
    RelayReservationAccepted,
}
