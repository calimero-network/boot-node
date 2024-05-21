use std::collections::hash_map::{self, HashMap};
use std::time::Duration;

use libp2p::futures::prelude::*;
use libp2p::swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p::{
    dcutr, gossipsub, identify, identity, mdns, noise, ping, relay, rendezvous, yamux, PeerId,
};
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
    mdns: mdns::tokio::Behaviour,
    ping: ping::Behaviour,
    rendezvous: rendezvous::client::Behaviour,
    relay: relay::client::Behaviour,
}

pub async fn run(
    keypair: identity::Keypair,
    port: u16,
    rendezvous_namespace: rendezvous::Namespace,
    relay_addresses: Vec<Multiaddr>,
    rendezvous_addresses: Vec<Multiaddr>,
) -> eyre::Result<(NetworkClient, mpsc::Receiver<types::NetworkEvent>)> {
    let mut rendezvous = HashMap::new();
    for address in &rendezvous_addresses {
        let entry = match peek_peer_id(&address) {
            Ok(peer_id) => (peer_id, RendezvousEntry::new(address.clone())),
            Err(err) => {
                eyre::bail!("Failed to parse rendezvous PeerId: {}", err);
            }
        };
        rendezvous.insert(entry.0, entry.1);
    }

    let mut relays = HashMap::new();
    for address in &relay_addresses {
        let entry = match peek_peer_id(&address) {
            Ok(peer_id) => (peer_id, RelayEntry::new(address.clone())),
            Err(err) => {
                eyre::bail!("Failed to parse relay PeerId: {}", err);
            }
        };
        relays.insert(entry.0, entry.1);
    }

    let (client, event_receiver, event_loop) =
        init(keypair, relays, rendezvous_namespace, rendezvous).await?;
    tokio::spawn(event_loop.run());

    let swarm_listen: Vec<Multiaddr> = vec![
        format!("/ip4/0.0.0.0/udp/{}/quic-v1", port).parse()?,
        format!("/ip4/0.0.0.0/tcp/{}", port).parse()?,
    ];

    for addr in swarm_listen {
        client.listen_on(addr).await?;
    }

    tokio::spawn(run_init_dial(
        client.clone(),
        rendezvous_addresses,
        relay_addresses,
    ));

    Ok((client, event_receiver))
}

async fn run_init_dial(
    client: NetworkClient,
    rendezvous_addresses: Vec<Multiaddr>,
    relay_addresses: Vec<Multiaddr>,
) {
    tokio::time::sleep(Duration::from_secs(5)).await;

    info!("Initiating dial to rendezvous and relay addresses.");

    for addr in rendezvous_addresses
        .into_iter()
        .chain(relay_addresses)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
    {
        info!("Dialing address: {:?}", addr);
        if let Err(err) = client.dial(addr).await {
            error!("Failed to dial rendezvous address: {}", err);
        };
    }
}

async fn init(
    keypair: identity::Keypair,
    relays: HashMap<PeerId, RelayEntry>,
    rendezvous_namespace: rendezvous::Namespace,
    rendezvous: HashMap<PeerId, RendezvousEntry>,
) -> eyre::Result<(
    NetworkClient,
    mpsc::Receiver<types::NetworkEvent>,
    EventLoop,
)> {
    let peer_id = keypair.public().to_peer_id();
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
            dcutr: dcutr::Behaviour::new(peer_id.clone()),
            identify: identify::Behaviour::new(
                identify::Config::new(PROTOCOL_VERSION.to_owned(), keypair.public())
                    .with_push_listen_addr_updates(true),
            ),
            gossipsub: gossipsub::Behaviour::new(
                gossipsub::MessageAuthenticity::Signed(keypair.clone()),
                gossipsub::Config::default(),
            )
            .expect("Valid gossipsub config."),
            mdns: mdns::Behaviour::new(mdns::Config::default(), peer_id.clone())
                .expect("Valid mdns config."),
            ping: ping::Behaviour::default(),
            rendezvous: rendezvous::client::Behaviour::new(keypair.clone()),
            relay: relay_behaviour,
        })?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(time::Duration::from_secs(30)))
        .build();

    let (command_sender, command_receiver) = mpsc::channel(32);
    let (event_sender, event_receiver) = mpsc::channel(32);

    let client = NetworkClient {
        sender: command_sender,
    };

    let event_loop = EventLoop::new(
        swarm,
        command_receiver,
        event_sender,
        relays,
        rendezvous_namespace,
        rendezvous,
    );

    Ok((client, event_receiver, event_loop))
}

pub(crate) struct EventLoop {
    swarm: Swarm<Behaviour>,
    command_receiver: mpsc::Receiver<Command>,
    event_sender: mpsc::Sender<types::NetworkEvent>,
    relays: HashMap<PeerId, RelayEntry>,
    rendezvous_namespace: rendezvous::Namespace,
    rendezvous: HashMap<PeerId, RendezvousEntry>,
    pending_dial: HashMap<PeerId, oneshot::Sender<eyre::Result<Option<()>>>>,
}

#[derive(Debug)]
pub(crate) struct RelayEntry {
    address: Multiaddr,
    identify_state: IdentifyState,
    reservation_state: RelayReservationState,
}

impl RelayEntry {
    pub(crate) fn new(address: Multiaddr) -> Self {
        Self {
            address,
            identify_state: Default::default(),
            reservation_state: Default::default(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct RendezvousEntry {
    address: Multiaddr,
    cookie: Option<rendezvous::Cookie>,
    identify_state: IdentifyState,
}

impl RendezvousEntry {
    pub(crate) fn new(address: Multiaddr) -> Self {
        Self {
            address,
            cookie: Default::default(),
            identify_state: Default::default(),
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub(crate) enum RelayReservationState {
    #[default]
    Unknown,
    Requested,
    Acquired,
}

#[derive(Debug, Default)]
pub(crate) struct IdentifyState {
    sent: bool,
    received: bool,
}

impl IdentifyState {
    pub(crate) fn is_exchanged(&self) -> bool {
        self.sent && self.received
    }
}

impl EventLoop {
    fn new(
        swarm: Swarm<Behaviour>,
        command_receiver: mpsc::Receiver<Command>,
        event_sender: mpsc::Sender<types::NetworkEvent>,
        relays: HashMap<PeerId, RelayEntry>,
        rendezvous_namespace: rendezvous::Namespace,
        rendezvous: HashMap<PeerId, RendezvousEntry>,
    ) -> Self {
        Self {
            swarm,
            command_receiver,
            event_sender,
            relays,
            rendezvous,
            rendezvous_namespace,
            pending_dial: Default::default(),
        }
    }

    pub(crate) async fn run(mut self) {
        let mut relays_dial_tick = tokio::time::interval(Duration::from_secs(90));
        let mut rendezvous_discover_tick = tokio::time::interval(Duration::from_secs(30));
        let mut rendezvous_dial_tick = tokio::time::interval(Duration::from_secs(90));

        loop {
            tokio::select! {
                event = self.swarm.next() => self.handle_swarm_event(event.expect("Swarm stream to be infinite.")).await,
                command = self.command_receiver.recv() => {
                    let Some(c) = command else { break };
                    self.handle_command(c).await;
                }
                _ = relays_dial_tick.tick() => self.handle_relays_dial().await,
                _ = rendezvous_dial_tick.tick() => self.handle_rendezvous_dial().await,
                _ = rendezvous_discover_tick.tick() => self.handle_rendezvous_discover().await,
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
                let peer_id = match peek_peer_id(&peer_addr) {
                    Ok(peer_id) => peer_id,
                    Err(e) => {
                        let _ = sender.send(Err(eyre::eyre!(e)));
                        return;
                    }
                };

                match self.pending_dial.entry(peer_id) {
                    hash_map::Entry::Occupied(_) => {
                        let _ = sender.send(Ok(None));
                    }
                    hash_map::Entry::Vacant(entry) => {
                        match self.swarm.dial(peer_addr) {
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
    Publish {
        topic: gossipsub::TopicHash,
        data: Vec<u8>,
        sender: oneshot::Sender<eyre::Result<gossipsub::MessageId>>,
    },
    PeerInfo {
        sender: oneshot::Sender<PeerInfo>,
    },
    MeshPeerCount {
        topic: gossipsub::TopicHash,
        sender: oneshot::Sender<MeshPeerInfo>,
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

pub(crate) fn peek_peer_id(address: &Multiaddr) -> eyre::Result<PeerId> {
    match address.iter().last() {
        Some(proto) => match proto {
            multiaddr::Protocol::P2p(peer_id) => Ok(peer_id),
            proto => Err(eyre::eyre!("expected p2p proto, got: {}", proto)),
        },
        None => Err(eyre::eyre!("expected at least one protocol")),
    }
}
