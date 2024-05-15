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
    pending_dial: HashMap<PeerId, DialEntry>,
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
                let dial_preference = match DialPreference::try_from(&peer_addr) {
                    Ok(preference) => preference,
                    Err(e) => {
                        let _ = sender.send(Err(eyre::eyre!(e)));
                        return;
                    }
                };

                match self.pending_dial.entry(*dial_preference.peer_id()) {
                    hash_map::Entry::Occupied(_) => {
                        let _ = sender.send(Ok(None));
                    }
                    hash_map::Entry::Vacant(entry) => {
                        match self.swarm.dial(peer_addr) {
                            Ok(_) => {
                                entry.insert(DialEntry {
                                    sender,
                                    dial_state: dial_preference.into(),
                                });
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
pub(crate) struct DialEntry {
    pub(crate) sender: oneshot::Sender<eyre::Result<Option<()>>>,
    pub(crate) dial_state: PendingDial,
}

#[derive(Debug)]
pub(crate) enum DialPreference {
    Direct {
        peer_id: PeerId,
    },
    Dcutr {
        peer_id: PeerId,
        relay_peer_id: PeerId,
    },
}

#[derive(Debug)]
pub(crate) enum PendingDial {
    Direct,
    Dcutr(PendingDialDcutr),
}

#[derive(Debug)]
pub(crate) struct PendingDialDcutr {
    pub(crate) relay_peer_id: PeerId,
    pub(crate) state: PendingDialDcutrState,
}

#[derive(Debug, PartialEq)]
pub(crate) enum PendingDialDcutrState {
    Initial,
    RelayConnected,
}

impl TryFrom<&Multiaddr> for DialPreference {
    type Error = &'static str;

    fn try_from(value: &Multiaddr) -> Result<Self, Self::Error> {
        // If there's no p2p-circuit protocol, directly extract peer_id
        if !value
            .iter()
            .any(|protocol| matches!(protocol, multiaddr::Protocol::P2pCircuit))
        {
            for protocol in value.iter() {
                if let multiaddr::Protocol::P2p(peer_id) = protocol {
                    return Ok(DialPreference::Direct { peer_id });
                }
            }
        }

        // Iterate over the protocols in the MultiAddr
        let mut iter = value.iter();
        let mut p2p_circuit_state = false;
        let mut peer_id = None;
        let mut relay_peer_id = None;

        while let Some(protocol) = iter.next() {
            match protocol {
                multiaddr::Protocol::P2pCircuit => {
                    // Found the p2p-circuit protocol,
                    // move into state where next expected peer_id is actual peer_id
                    p2p_circuit_state = true;
                }
                multiaddr::Protocol::P2p(id) => {
                    if p2p_circuit_state {
                        peer_id = Some(id);
                    } else {
                        relay_peer_id = Some(id);
                    };
                    if peer_id.is_some() && relay_peer_id.is_some() {
                        return Ok(DialPreference::Dcutr {
                            peer_id: peer_id.unwrap(),             // safe to unwrap
                            relay_peer_id: relay_peer_id.unwrap(), // safe to unwrap
                        });
                    }
                }
                _ => {}
            }
        }

        return Err("Failed to convert Multiaddr to DialPreference");
    }
}

impl From<DialPreference> for PendingDial {
    fn from(value: DialPreference) -> Self {
        match value {
            DialPreference::Direct { .. } => Self::Direct,
            DialPreference::Dcutr { relay_peer_id, .. } => Self::Dcutr(PendingDialDcutr {
                relay_peer_id,
                state: PendingDialDcutrState::Initial,
            }),
        }
    }
}

impl DialPreference {
    fn is_direct(&self) -> bool {
        matches!(self, Self::Direct { .. })
    }

    fn is_dcutr(&self) -> bool {
        matches!(self, Self::Dcutr { .. })
    }

    fn peer_id(&self) -> &PeerId {
        match self {
            Self::Direct { peer_id } => peer_id,
            Self::Dcutr { peer_id, .. } => peer_id,
        }
    }

    fn relay_peer_id(&self) -> Option<&PeerId> {
        match self {
            Self::Direct { .. } => None,
            Self::Dcutr { relay_peer_id, .. } => Some(relay_peer_id),
        }
    }
}
