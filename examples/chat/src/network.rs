use std::collections::hash_map::{self, HashMap};
use std::time::Duration;

use libp2p::futures::prelude::*;
use libp2p::swarm::{NetworkBehaviour, Swarm, SwarmEvent};
use libp2p::{
    dcutr, gossipsub, identify, identity, kad, mdns, noise, ping, relay, rendezvous, yamux, PeerId,
};
use multiaddr::Multiaddr;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tracing::{debug, trace, warn};

pub mod client;
pub mod discovery;
pub mod events;
pub mod types;

use client::NetworkClient;

const PROTOCOL_VERSION: &str = concat!("/", env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
const CALIMERO_KAD_PROTO_NAME: libp2p::StreamProtocol =
    libp2p::StreamProtocol::new("/calimero/kad/1.0.0");

#[derive(NetworkBehaviour)]
struct Behaviour {
    dcutr: dcutr::Behaviour,
    gossipsub: gossipsub::Behaviour,
    identify: identify::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
    mdns: mdns::tokio::Behaviour,
    ping: ping::Behaviour,
    rendezvous: rendezvous::client::Behaviour,
    relay: relay::client::Behaviour,
}

pub async fn run(
    keypair: identity::Keypair,
    port: u16,
    boot_nodes: Vec<Multiaddr>,
    rendezvous_namespace: rendezvous::Namespace,
) -> eyre::Result<(NetworkClient, mpsc::Receiver<types::NetworkEvent>)> {
    let (client, event_receiver, event_loop) =
        init(keypair, boot_nodes, rendezvous_namespace).await?;

    tokio::spawn(event_loop.run());

    let swarm_listen: Vec<Multiaddr> = vec![
        format!("/ip4/0.0.0.0/udp/{}/quic-v1", port).parse()?,
        format!("/ip4/0.0.0.0/tcp/{}", port).parse()?,
    ];

    for addr in swarm_listen {
        client.listen_on(addr).await?;
    }

    Ok((client, event_receiver))
}

async fn init(
    keypair: identity::Keypair,
    boot_nodes: Vec<Multiaddr>,
    rendezvous_namespace: rendezvous::Namespace,
) -> eyre::Result<(
    NetworkClient,
    mpsc::Receiver<types::NetworkEvent>,
    EventLoop,
)> {
    let bootstrap_peers = {
        let mut peers = vec![];

        for mut addr in boot_nodes {
            let Some(multiaddr::Protocol::P2p(peer_id)) = addr.pop() else {
                eyre::bail!("Failed to parse peer id from addr {:?}", addr);
            };

            peers.push((peer_id, addr));
        }

        peers
    };

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
            kad: {
                let mut kademlia_config = kad::Config::default();
                kademlia_config.set_protocol_names(vec![CALIMERO_KAD_PROTO_NAME]);

                let mut kademlia = kad::Behaviour::with_config(
                    peer_id,
                    kad::store::MemoryStore::new(peer_id),
                    kademlia_config,
                );

                kademlia.set_mode(Some(kad::Mode::Client));

                for (peer_id, addr) in bootstrap_peers {
                    kademlia.add_address(&peer_id, addr);
                }
                if let Err(err) = kademlia.bootstrap() {
                    warn!(%err, "Failed to bootstrap Kademlia");
                };

                kademlia
            },
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

    let event_loop = EventLoop::new(swarm, command_receiver, event_sender, rendezvous_namespace);

    Ok((client, event_receiver, event_loop))
}

pub(crate) struct EventLoop {
    swarm: Swarm<Behaviour>,
    command_receiver: mpsc::Receiver<Command>,
    event_sender: mpsc::Sender<types::NetworkEvent>,
    discovery: discovery::Discovery,
    pending_dial: HashMap<PeerId, oneshot::Sender<eyre::Result<Option<()>>>>,
}

impl EventLoop {
    fn new(
        swarm: Swarm<Behaviour>,
        command_receiver: mpsc::Receiver<Command>,
        event_sender: mpsc::Sender<types::NetworkEvent>,
        rendezvous_namespace: rendezvous::Namespace,
    ) -> Self {
        Self {
            swarm,
            command_receiver,
            event_sender,
            discovery: discovery::Discovery::new(discovery::RendezvousConfig::new(
                rendezvous_namespace,
                Duration::from_secs(90),
                0.5,
            )),
            pending_dial: Default::default(),
        }
    }

    pub(crate) async fn run(mut self) {
        let mut rendezvous_discover_tick =
            tokio::time::interval(self.discovery.rendezvous_config.discovery_interval);

        loop {
            tokio::select! {
                event = self.swarm.next() => self.handle_swarm_event(event.expect("Swarm stream to be infinite.")).await,
                command = self.command_receiver.recv() => {
                    let Some(c) = command else { break };
                    self.handle_command(c).await;
                }
                _ = rendezvous_discover_tick.tick() => self.handle_rendezvous_discoveries().await,
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
            Command::PeersInfo { sender } => {
                let peers = self
                    .swarm
                    .connected_peers()
                    .into_iter()
                    .map(|peer| peer.clone())
                    .collect::<Vec<_>>();
                let count = peers.len();

                let discovered_peers = self
                    .discovery
                    .state
                    .get_peers()
                    .map(|(id, peer)| (id.clone(), peer.clone()))
                    .collect::<Vec<_>>();
                let discovered_count = discovered_peers.len();

                let _ = sender.send(PeersInfo {
                    count,
                    peers,
                    discovered_count,
                    discovered_peers,
                });
            }
            Command::MeshPeersCount { topic, sender } => {
                let peers = self
                    .swarm
                    .behaviour_mut()
                    .gossipsub
                    .mesh_peers(&topic)
                    .map(|peer| peer.clone())
                    .collect::<Vec<_>>();
                let count = peers.len();

                let _ = sender.send(MeshPeersInfo { count, peers });
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
    PeersInfo {
        sender: oneshot::Sender<PeersInfo>,
    },
    MeshPeersCount {
        topic: gossipsub::TopicHash,
        sender: oneshot::Sender<MeshPeersInfo>,
    },
}

#[allow(dead_code)] // Info structs for pretty printing
#[derive(Debug)]
pub(crate) struct PeersInfo {
    count: usize,
    peers: Vec<PeerId>,
    discovered_count: usize,
    discovered_peers: Vec<(PeerId, discovery::state::PeerInfo)>,
}

#[allow(dead_code)] // Info structs for pretty printing
#[derive(Debug)]
pub(crate) struct MeshPeersInfo {
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
