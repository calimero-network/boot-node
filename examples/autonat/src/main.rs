use std::{error::Error, net::Ipv4Addr, time::Duration};

use clap::Parser;
use futures::StreamExt;
use libp2p::relay::{self, HOP_PROTOCOL_NAME};
use libp2p::{
    autonat::{self, NatStatus},
    core::{multiaddr::Protocol, Multiaddr},
    identify,
    identity::Keypair,
    noise, rendezvous,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, PeerId,
};
use tracing::debug;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[clap(name = "libp2p autonat")]
struct Opt {
    #[clap(long)]
    listen_port: Option<u16>,

    #[clap(long)]
    server_address: Option<Multiaddr>,

    #[clap(long)]
    server_peer_id: Option<PeerId>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("libp2p=debug".parse()?)
                .add_directive("libp2p_core::transport::choice=warn".parse()?),
        )
        .try_init();

    let opt = Opt::parse();
    let rendezvous_point = "12D3KooWMgoF9xzyeKJHtRvrYwdomheRbHPELagWZwTLmXb6bCVC".parse()?;

    // Setup swarm
    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|key, relay_behaviour| Behaviour::new(key.clone(), relay_behaviour))?
        .build();

    swarm.listen_on(
        Multiaddr::empty()
            .with(Protocol::Ip4(Ipv4Addr::UNSPECIFIED))
            .with(Protocol::Tcp(opt.listen_port.unwrap_or(0))),
    )?;

    // Configure servers
    if let Some(server_peer_id) = opt.server_peer_id {
        swarm
            .behaviour_mut()
            .auto_nat
            .add_server(server_peer_id, opt.server_address.clone());
    }

    // Boot node / Rendezvous point
    let rendezvous_point_address = "/ip4/18.156.18.6/tcp/4001".parse::<Multiaddr>()?;
    swarm
        .behaviour_mut()
        .auto_nat
        .add_server(rendezvous_point, Some(rendezvous_point_address));

    // IPFS node
    swarm.behaviour_mut().auto_nat.add_server(
        "QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ".parse()?,
        Some("/ip4/104.131.131.82/tcp/4001".parse()?),
    );

    let mut registered = false;
    let mut relayed = false;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                println!("Listening on {address:?}");

                // Check if this is a relay address and attempt registration
                if !registered && address.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
                    if let Err(error) = register_with_rendezvous(&mut swarm, rendezvous_point) {
                        println!("Failed to register: {error}");
                    } else {
                        registered = true;
                    }
                }
            }
            SwarmEvent::Behaviour(event) => match event {
                BehaviourEvent::Identify(event) => {
                    println!("Identify: {:#?}", event);
                    if let identify::Event::Received { peer_id, info, .. } = event {
                        handle_identify(&mut swarm, peer_id, info, &mut relayed);
                    }
                }
                BehaviourEvent::AutoNat(autonat_event) => {
                    match autonat_event {
                        autonat::Event::InboundProbe(inbound_probe_event) => {
                            println!("Inbound probe: {:#?}", inbound_probe_event);
                        }
                        autonat::Event::OutboundProbe(outbound_probe_event) => {
                            // Print details about the probe
                            match &outbound_probe_event {
                                autonat::OutboundProbeEvent::Request { peer, .. } => {
                                    println!("Sent probe request to peer: {}", peer);
                                }
                                autonat::OutboundProbeEvent::Response { peer, address, .. } => {
                                    println!("Received probe response from peer: {}", peer);
                                    println!("  → Determined external address: {}", address);
                                }
                                autonat::OutboundProbeEvent::Error { peer, error, .. } => {
                                    println!("Probe error with peer {:?}: {:?}", peer, error);
                                }
                            }

                            // Check and print current confidence
                            let confidence = swarm.behaviour().auto_nat.confidence();
                            let status = swarm.behaviour().auto_nat.nat_status();
                            println!(
                                "Current AutoNAT confidence: {}\nStatus: {:?}",
                                confidence, status
                            );

                            // Register with rendezvous if we're public with high confidence
                            if confidence >= 2
                                && matches!(status, NatStatus::Public(_))
                                && !registered
                            {
                                println!(
                                    "Confidence over 2, registering peerID at rendezvous point"
                                );
                                if let Err(error) =
                                    register_with_rendezvous(&mut swarm, rendezvous_point)
                                {
                                    println!("Failed to register: {error}");
                                } else {
                                    registered = true;
                                }
                            }
                        }
                        autonat::Event::StatusChanged { old, new } => {
                            println!("NAT status changed from {:?} to {:?}", old, new);

                            match &new {
                                autonat::NatStatus::Public(addr) => {
                                    println!("Node is publicly reachable at: {:?}", addr);
                                    // We don't register here - waiting for more confirmations
                                }
                                autonat::NatStatus::Private => {
                                    println!("Node is behind NAT/firewall (private network)");
                                    if registered && matches!(old, NatStatus::Public(_)) {
                                        swarm.behaviour_mut().rendezvous.unregister(
                                            rendezvous::Namespace::from_static("rendezvous"),
                                            rendezvous_point,
                                        );
                                        registered = false;
                                    }
                                }
                                autonat::NatStatus::Unknown => {
                                    println!("NAT status is currently unknown");
                                }
                            }
                        }
                    }
                }
                BehaviourEvent::Rendezvous(rendezvous_event) => {
                    debug!("Rendezvous event: {:?}", rendezvous_event);
                }
                BehaviourEvent::Relay(relay_event) => {
                    debug!("Relay event: {:?}", relay_event);
                }
            },
            e => println!("{e:?}"),
        }
    }
}

fn handle_identify(
    swarm: &mut libp2p::Swarm<Behaviour>,
    peer_id: PeerId,
    info: identify::Info,
    relayed: &mut bool,
) {
    let status = swarm.behaviour().auto_nat.nat_status();

    for protocol in info.protocols {
        if protocol == HOP_PROTOCOL_NAME && matches!(status, NatStatus::Private) && !*relayed {
            println!(
                "Found relay capability in peer {}, creating reservation",
                peer_id
            );

            if !info.listen_addrs.is_empty() {
                setup_relay_address(swarm, peer_id, &info.listen_addrs[0], relayed);
            } else {
                println!(
                    "Peer {} supports relay but has no listen addresses",
                    peer_id
                );
            }
        }
    }
}

fn setup_relay_address(
    swarm: &mut libp2p::Swarm<Behaviour>,
    peer_id: PeerId,
    relay_addr: &Multiaddr,
    relayed: &mut bool,
) {
    // Create a relay address
    let relayed_addr = relay_addr
        .clone()
        .with(Protocol::P2p(peer_id))
        .with(Protocol::P2pCircuit)
        .with(Protocol::P2p(*swarm.local_peer_id()));

    match swarm.listen_on(relayed_addr.clone()) {
        Ok(listener_id) => {
            println!(
                "Successfully created relay reservation with listener ID: {:?}",
                listener_id
            );
            swarm.add_external_address(relayed_addr);
            *relayed = true;
        }
        Err(err) => {
            println!("Failed to create relay reservation: {:?}", err);
        }
    }
}

fn register_with_rendezvous(
    swarm: &mut libp2p::Swarm<Behaviour>,
    rendezvous_point: PeerId,
) -> Result<(), Box<dyn Error>> {
    swarm.behaviour_mut().rendezvous.register(
        rendezvous::Namespace::from_static("rendezvous"),
        rendezvous_point,
        None,
    )?;
    println!("Successfully registered with rendezvous point");
    Ok(())
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    identify: identify::Behaviour,
    auto_nat: autonat::Behaviour,
    rendezvous: rendezvous::client::Behaviour,
    relay: relay::client::Behaviour,
}

impl Behaviour {
    fn new(key: Keypair, relay_behaviour: relay::client::Behaviour) -> Self {
        Self {
            identify: identify::Behaviour::new(identify::Config::new(
                "/ipfs/0.1.0".into(),
                key.public().clone(),
            )),
            auto_nat: autonat::Behaviour::new(
                key.public().to_peer_id(),
                autonat::Config {
                    retry_interval: Duration::from_secs(10),
                    refresh_interval: Duration::from_secs(30),
                    boot_delay: Duration::from_secs(5),
                    throttle_server_period: Duration::ZERO,
                    only_global_ips: true,
                    ..Default::default()
                },
            ),
            rendezvous: rendezvous::client::Behaviour::new(key.clone()),
            relay: relay_behaviour,
        }
    }
}
