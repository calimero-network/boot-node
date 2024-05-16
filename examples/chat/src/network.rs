use std::collections::hash_map::{self, HashMap};
use std::time::Duration;

use libp2p::futures::prelude::*;
use libp2p::swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p::{dcutr, gossipsub, identify, identity, noise, ping, relay, yamux, PeerId};
use multiaddr::Multiaddr;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tracing::{debug, error, info, trace, warn};

pub mod client;
pub mod events;
pub mod types;

use client::NetworkClient;

const PROTOCOL_VERSION: &str = concat!("/", env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

#[derive(NetworkBehaviour)]
struct Behaviour {
    dcutr: dcutr::Behaviour,
    identify: identify::Behaviour,
    gossipsub: gossipsub::Behaviour,
    ping: ping::Behaviour,
    relay_client: relay::client::Behaviour,
}

pub async fn run(
    keypair: identity::Keypair,
    port: u16,
    relay_address: Multiaddr,
) -> eyre::Result<(NetworkClient, mpsc::Receiver<types::NetworkEvent>)> {
    let (client, mut event_receiver, event_loop) = init(keypair).await?;

    tokio::spawn(event_loop.run());

    let swarm_listen: Vec<Multiaddr> = vec![
        format!("/ip4/0.0.0.0/udp/{}/quic-v1", port).parse()?,
        format!("/ip4/0.0.0.0/tcp/{}", port).parse()?,
    ];
    for addr in swarm_listen {
        client.listen_on(addr.clone()).await?;
    }

    // Reference: https://github.com/libp2p/rust-libp2p/blob/60fd566a955a33c42a6ab6eefc1f0fedef9f8b83/examples/dcutr/src/main.rs#L118
    loop {
        tokio::select! {
            Some(event) = event_receiver.recv() => {
                match event {
                    types::NetworkEvent::ListeningOn { address, .. } => {
                        info!("Listening on: {}", address);
                    }
                    _ => {
                        error!("Recieved unexpected network event: {:?}", event)
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                // Likely listening on all interfaces now, thus continuing by breaking the loop.
                break;
            }
        }
    }

    // Connect to the relay server. Not for the reservation or relayed connection, but to (a) learn
    // our local public address and (b) enable a freshly started relay to learn its public address.
    client.dial(relay_address.clone()).await?;

    let mut learned_observed_addr = false;
    let mut told_relay_observed_addr = false;
    let relay_peer_id = match relay_address.clone().iter().find_map(|protocol| {
        if let libp2p::multiaddr::Protocol::P2p(peer_id) = protocol {
            Some(peer_id)
        } else {
            None
        }
    }) {
        Some(peer_id) => peer_id,
        None => eyre::bail!("Failed to get PeerId from relay address"),
    };

    loop {
        match event_receiver.recv().await.unwrap() {
            types::NetworkEvent::IdentifySent { peer_id } => {
                if peer_id == relay_peer_id {
                    info!("Told relay its public address");
                    told_relay_observed_addr = true;
                }
            }
            types::NetworkEvent::IdentifyReceived {
                peer_id,
                observed_addr,
            } => {
                if peer_id == relay_peer_id {
                    info!("Relay told us our observed address: {}", observed_addr);
                    learned_observed_addr = true;
                }
            }
            event => info!("unexpected: {event:?}"),
        };

        if learned_observed_addr && told_relay_observed_addr {
            break;
        }
    }

    // Create reservation on relay server and wait for it to be accepted ...
    client
        .listen_on(relay_address.with(multiaddr::Protocol::P2pCircuit))
        .await?;

    loop {
        match event_receiver.recv().await.unwrap() {
            types::NetworkEvent::RelayReservationAccepted => {
                info!("Relay accepted our reservation");
                break;
            }
            event => info!("unexpected: {event:?}"),
        };
    }

    // ... and now wait until we are listening on the "relayed" interfaces
    // Reference: https://github.com/libp2p/rust-libp2p/blob/60fd566a955a33c42a6ab6eefc1f0fedef9f8b83/examples/dcutr/src/main.rs#L118
    loop {
        tokio::select! {
            Some(event) = event_receiver.recv() => {
                match event {
                    types::NetworkEvent::ListeningOn { address, .. } => {
                        info!("Listening on: {}", address);
                    }
                    _ => {
                        error!("Recieved unexpected network event: {:?}", event)
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                // Likely listening on all interfaces now, thus continuing by breaking the loop.
                break;
            }
        }
    }

    Ok((client, event_receiver))
}

async fn init(
    keypair: identity::Keypair,
) -> eyre::Result<(
    NetworkClient,
    mpsc::Receiver<types::NetworkEvent>,
    EventLoop,
)> {
    let swarm = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            Default::default(),
            (libp2p::tls::Config::new, libp2p::noise::Config::new),
            libp2p::yamux::Config::default,
        )?
        .with_quic()
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|keypair, relay_behaviour| Behaviour {
            dcutr: dcutr::Behaviour::new(keypair.public().to_peer_id()),
            identify: identify::Behaviour::new(
                identify::Config::new(PROTOCOL_VERSION.to_owned(), keypair.public())
                    .with_push_listen_addr_updates(true),
            ),
            gossipsub: gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(keypair.clone()),
                gossipsub::Config::default(),
            )
            .expect("Valid gossipsub config."),
            ping: ping::Behaviour::default(),
            relay_client: relay_behaviour,
        })?
        .with_swarm_config(|cfg| {
            cfg.with_idle_connection_timeout(time::Duration::from_secs(u64::MAX))
        })
        .build();

    let (command_sender, command_receiver) = mpsc::channel(32);
    let (event_sender, event_receiver) = mpsc::channel(32);

    let client = NetworkClient {
        sender: command_sender,
    };

    let event_loop = EventLoop::new(swarm, command_receiver, event_sender);

    Ok((client, event_receiver, event_loop))
}

pub(crate) struct EventLoop {
    swarm: Swarm<Behaviour>,
    command_receiver: mpsc::Receiver<Command>,
    event_sender: mpsc::Sender<types::NetworkEvent>,
    pending_dial: HashMap<PeerId, oneshot::Sender<eyre::Result<Option<()>>>>,
}

impl EventLoop {
    fn new(
        swarm: Swarm<Behaviour>,
        command_receiver: mpsc::Receiver<Command>,
        event_sender: mpsc::Sender<types::NetworkEvent>,
    ) -> Self {
        Self {
            swarm,
            command_receiver,
            event_sender,
            pending_dial: Default::default(),
        }
    }

    pub(crate) async fn run(mut self) {
        loop {
            tokio::select! {
                event = self.swarm.next() => self.handle_swarm_event(event.expect("Swarm stream to be infinite.")).await,
                command = self.command_receiver.recv() => {
                    let Some(c) = command else { break };
                    self.handle_command(c).await;
                }
            }
        }
    }

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::ListenOn { addr, sender } => {
                let _ = match self.swarm.listen_on(addr) {
                    Ok(_) => sender.send(Ok(())),
                    Err(e) => sender.send(Err(eyre::eyre!(e))),
                };
            }
            Command::Dial { peer_addr, sender } => {
                let addr_meta = match MultiaddrMeta::try_from(&peer_addr) {
                    Ok(meta) => meta,
                    Err(e) => {
                        let _ = sender.send(Err(eyre::eyre!(e)));
                        return;
                    }
                };

                match self.pending_dial.entry(*addr_meta.peer_id()) {
                    hash_map::Entry::Occupied(_) => {
                        let _ = sender.send(Ok(None));
                    }
                    hash_map::Entry::Vacant(entry) => {
                        match self.swarm.dial(peer_addr.clone()) {
                            Ok(_) => {
                                entry.insert(sender);
                            }
                            Err(err) => {
                                let _ = sender.send(Err(eyre::eyre!(err)));
                            }
                        };
                    }
                }
            }
            Command::Subscribe { topic, sender } => {
                if let Err(err) = self.swarm.behaviour_mut().gossipsub.subscribe(&topic) {
                    let _ = sender.send(Err(eyre::eyre!(err)));
                    return;
                }

                let _ = sender.send(Ok(topic));
            }
            Command::Unsubscribe { topic, sender } => {
                if let Err(err) = self.swarm.behaviour_mut().gossipsub.unsubscribe(&topic) {
                    let _ = sender.send(Err(eyre::eyre!(err)));
                    return;
                }

                let _ = sender.send(Ok(topic));
            }
            Command::PeerInfo { sender } => {
                let peers: Vec<PeerId> = self
                    .swarm
                    .connected_peers()
                    .into_iter()
                    .map(|peer| peer.clone())
                    .collect();
                let count = peers.len();

                let _ = sender.send(PeerInfo { count, peers });
            }
            Command::MeshPeerCount { topic, sender } => {
                let peers: Vec<PeerId> = self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .mesh_peers(&topic)
                    .map(|peer| peer.clone())
                    .collect();
                let count = peers.len();

                let _ = sender.send(MeshPeerInfo { count, peers });
            }
            Command::Publish {
                topic,
                data,
                sender,
            } => {
                let id = match self.swarm.behaviour_mut().gossipsub.publish(topic, data) {
                    Ok(id) => id,
                    Err(err) => {
                        let _ = sender.send(Err(eyre::eyre!(err)));
                        return;
                    }
                };

                let _ = sender.send(Ok(id));
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum Command {
    ListenOn {
        addr: Multiaddr,
        sender: oneshot::Sender<eyre::Result<()>>,
    },
    Dial {
        peer_addr: Multiaddr,
        sender: oneshot::Sender<eyre::Result<Option<()>>>,
    },
    Subscribe {
        topic: gossipsub::IdentTopic,
        sender: oneshot::Sender<eyre::Result<gossipsub::IdentTopic>>,
    },
    Unsubscribe {
        topic: gossipsub::IdentTopic,
        sender: oneshot::Sender<eyre::Result<gossipsub::IdentTopic>>,
    },
    PeerInfo {
        sender: oneshot::Sender<PeerInfo>,
    },
    MeshPeerCount {
        topic: gossipsub::TopicHash,
        sender: oneshot::Sender<MeshPeerInfo>,
    },
    Publish {
        topic: gossipsub::TopicHash,
        data: Vec<u8>,
        sender: oneshot::Sender<eyre::Result<gossipsub::MessageId>>,
    },
}

#[allow(dead_code)] // Info structs for pretty printing
#[derive(Debug)]
pub(crate) struct PeerInfo {
    count: usize,
    peers: Vec<PeerId>,
}

#[allow(dead_code)] // Info structs for pretty printing
#[derive(Debug)]
pub(crate) struct MeshPeerInfo {
    count: usize,
    peers: Vec<PeerId>,
}

#[derive(Debug)]
pub(crate) struct MultiaddrMeta {
    peer_id: PeerId,
    relay_peer_ids: Vec<PeerId>,
}

impl TryFrom<&Multiaddr> for MultiaddrMeta {
    type Error = &'static str;

    fn try_from(value: &Multiaddr) -> Result<Self, Self::Error> {
        let mut peer_ids = Vec::new();

        let mut iter = value.iter();
        while let Some(protocol) = iter.next() {
            match protocol {
                multiaddr::Protocol::P2pCircuit => {
                    if peer_ids.is_empty() {
                        return Err("expected at least one p2p proto before P2pCircuit");
                    }
                    let Some(multiaddr::Protocol::P2p(id)) = iter.next() else {
                        return Err("expected p2p proto after P2pCircuit");
                    };
                    peer_ids.push(id);
                }
                multiaddr::Protocol::P2p(id) => {
                    peer_ids.push(id);
                }
                _ => {}
            }
        }

        match peer_ids.len() {
            0 => Err("expected at least one p2p proto"),
            _ => Ok(Self {
                peer_id: peer_ids[peer_ids.len() - 1],
                relay_peer_ids: peer_ids[0..peer_ids.len() - 1].to_vec(),
            }),
        }
    }
}

impl MultiaddrMeta {
    fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    fn is_relayed(&self) -> bool {
        !self.relay_peer_ids.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_multiaddr() {
        let addr_str = "/ip4/3.71.239.80/udp/4001/quic-v1/p2p/12D3KooWAgFah4EZtWnMMGMUddGdJpb5cq2NubNCAD2jA5AZgbXF/p2p-circuit/p2p/12D3KooWP285Hw3CSTdr9oU6Ezz4hDoi6XS5vfDjjNeTJ1uFMGvp/p2p-circuit/p2p/12D3KooWMpeKAbMK4BTPsQY3rG7XwtdstseHGcq7kffY8LToYYKK";
        let multiaddr: Multiaddr = addr_str.parse().expect("valid multiaddr");

        let meta = MultiaddrMeta::try_from(&multiaddr).expect("valid MultiaddrMeta");
        let expected_peer_id: PeerId = "12D3KooWMpeKAbMK4BTPsQY3rG7XwtdstseHGcq7kffY8LToYYKK"
            .parse()
            .expect("valid peer id");
        let relay_peer_ids: Vec<PeerId> = vec![
            "12D3KooWAgFah4EZtWnMMGMUddGdJpb5cq2NubNCAD2jA5AZgbXF"
                .parse()
                .expect("valid peer id"),
            "12D3KooWP285Hw3CSTdr9oU6Ezz4hDoi6XS5vfDjjNeTJ1uFMGvp"
                .parse()
                .expect("valid peer id"),
        ];

        assert_eq!(meta.peer_id, expected_peer_id);
        assert_eq!(meta.relay_peer_ids, relay_peer_ids);
        assert!(meta.is_relayed());
    }

    #[test]
    fn test_no_p2p_proto() {
        let addr_str = "/ip4/3.71.239.80/udp/4001/quic-v1";
        let multiaddr: Multiaddr = addr_str.parse().expect("valid multiaddr");

        let result = MultiaddrMeta::try_from(&multiaddr);
        assert!(result.is_err());
        assert_eq!(result.err(), Some("expected at least one p2p proto"));
    }

    #[test]
    fn test_p2p_circuit_without_previous_p2p() {
        let addr_str = "/ip4/3.71.239.80/udp/4001/quic-v1/p2p-circuit";
        let multiaddr: Multiaddr = addr_str.parse().expect("valid multiaddr");

        let result = MultiaddrMeta::try_from(&multiaddr);
        assert!(result.is_err());
        assert_eq!(
            result.err(),
            Some("expected at least one p2p proto before P2pCircuit")
        );
    }

    #[test]
    fn test_single_p2p_no_circuit() {
        let addr_str = "/ip4/3.71.239.80/udp/4001/quic-v1/p2p/12D3KooWAgFah4EZtWnMMGMUddGdJpb5cq2NubNCAD2jA5AZgbXF";
        let multiaddr: Multiaddr = addr_str.parse().expect("valid multiaddr");

        let meta = MultiaddrMeta::try_from(&multiaddr).expect("valid MultiaddrMeta");
        let expected_peer_id: PeerId = "12D3KooWAgFah4EZtWnMMGMUddGdJpb5cq2NubNCAD2jA5AZgbXF"
            .parse()
            .expect("valid peer id");

        assert_eq!(meta.peer_id, expected_peer_id);
        assert!(meta.relay_peer_ids.is_empty());
        assert!(!meta.is_relayed());
    }

    #[test]
    fn test_p2p_circuit_with_single_p2p() {
        let addr_str = "/ip4/3.71.239.80/udp/4001/quic-v1/p2p/12D3KooWAgFah4EZtWnMMGMUddGdJpb5cq2NubNCAD2jA5AZgbXF/p2p-circuit/p2p/12D3KooWP285Hw3CSTdr9oU6Ezz4hDoi6XS5vfDjjNeTJ1uFMGvp";
        let multiaddr: Multiaddr = addr_str.parse().expect("valid multiaddr");

        let meta = MultiaddrMeta::try_from(&multiaddr).expect("valid MultiaddrMeta");
        let expected_peer_id: PeerId = "12D3KooWP285Hw3CSTdr9oU6Ezz4hDoi6XS5vfDjjNeTJ1uFMGvp"
            .parse()
            .expect("valid peer id");
        let relay_peer_ids: Vec<PeerId> =
            vec!["12D3KooWAgFah4EZtWnMMGMUddGdJpb5cq2NubNCAD2jA5AZgbXF"
                .parse()
                .expect("valid peer id")];

        assert_eq!(meta.peer_id, expected_peer_id);
        assert_eq!(meta.relay_peer_ids, relay_peer_ids);
        assert!(meta.is_relayed());
    }
}
