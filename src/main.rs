use std::net::Ipv4Addr;

use clap::Parser;
use libp2p::futures::prelude::*;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, kad, ping, relay, rendezvous, Multiaddr, StreamProtocol, Swarm};
use libp2p_metrics::{Metrics, Recorder, Registry};
use tracing::info;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use calimero_network_primitives::autonat_v2;

mod http_service;

const PROTOCOL_VERSION: &str = concat!("/", env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
const CALIMERO_KAD_PROTO_NAME: StreamProtocol = StreamProtocol::new("/calimero/kad/1.0.0");
const MAX_RELAY_CIRCUIT_BYTES: u64 = 100 << 20; // 100 MiB

#[derive(NetworkBehaviour)]
struct Behaviour {
    autonat: autonat_v2::Behaviour,
    identify: identify::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
    ping: ping::Behaviour,
    relay: relay::Behaviour,
    rendezvous: rendezvous::server::Behaviour,
}

#[derive(Debug, Parser)]
#[clap(name = "calimero relay")]
struct Opt {
    /// The file with the protobuf encoded private key used to derive PeerId and sign network activity
    #[clap(long, value_name = "PRIVATE_KEY")]
    #[clap(env = "RELAY_SERVER_PRIVATE_KEY", hide_env_values = true)]
    private_key: Option<camino::Utf8PathBuf>,

    /// Generate an ephemeral keypair for development/debugging (ignored if --private-key is provided)
    #[clap(long)]
    dev: bool,

    /// The port used to listen on all interfaces
    #[clap(long, value_name = "PORT", default_value = "4001")]
    #[clap(env = "RELAY_SERVER_PORT", hide_env_values = true)]
    port: u16,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::builder().parse(format!(
            "info,{}",
            std::env::var("RUST_LOG").unwrap_or_default()
        ))?)
        .with(tracing_subscriber::fmt::layer())
        .init();

    let opt = Opt::parse();

    let keypair = match opt.private_key {
        Some(path) => {
            let bytes = std::fs::read(&path)?;
            identity::Keypair::from_protobuf_encoding(&bytes)?
        }
        None if opt.dev => {
            // Use a fixed seed for deterministic peer ID in dev mode
            // This ensures the same PeerId across restarts for easier development
            const DEV_SEED: [u8; 32] = *b"calimero-boot-node-dev-seed-key!";
            tracing::warn!("Using hardcoded dev keypair - do not use in production");
            identity::Keypair::ed25519_from_bytes(DEV_SEED)?
        }
        None => {
            eyre::bail!("Either --private-key or --dev must be provided");
        }
    };
    let peer_id = keypair.public().to_peer_id();

    info!("Peer id: {:?}", peer_id);

    let mut metric_registry = Registry::default();

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            Default::default(),
            (libp2p::tls::Config::new, libp2p::noise::Config::new),
            libp2p::yamux::Config::default,
        )?
        .with_quic()
        .with_bandwidth_metrics(&mut metric_registry)
        .with_behaviour(|keypair| {
            let mut autonat = autonat_v2::Behaviour::new(autonat_v2::Config::default());
            // Enable server mode since boot-node is intended to be publicly reachable
            autonat
                .enable_server()
                .expect("Server should enable on fresh behaviour");

            Behaviour {
                autonat,
                identify: identify::Behaviour::new(identify::Config::new(
                    PROTOCOL_VERSION.to_owned(),
                    keypair.public(),
                )),
                kad: {
                    let mut kademlia_config = kad::Config::new(CALIMERO_KAD_PROTO_NAME);
                    // Instantly remove records and provider records.
                    // TODO: figure out what to do with these values, ref: https://github.com/libp2p/rust-libp2p/blob/1aa016e1c7e3976748a726eab37af44d1c5b7a6e/misc/server/src/behaviour.rs#L38
                    kademlia_config.set_record_ttl(Some(std::time::Duration::from_secs(0)));
                    kademlia_config
                        .set_provider_record_ttl(Some(std::time::Duration::from_secs(0)));

                    let mut kademlia = kad::Behaviour::with_config(
                        peer_id,
                        kad::store::MemoryStore::new(peer_id),
                        kademlia_config,
                    );
                    kademlia.set_mode(Some(kad::Mode::Server));
                    // TODO: implement support for adding bootstrap peers
                    // for peer in opt.bootstrap_peers.iter() {
                    //     kademlia.add_address(&PeerId::from_str(peer).unwrap(), bootaddr.clone());
                    // }
                    // if let Err(err) = kademlia.bootstrap() {
                    //     warn!(%err, "Failed to bootstrap Kademlia");
                    // };

                    kademlia
                },
                ping: ping::Behaviour::new(ping::Config::new()),
                rendezvous: rendezvous::server::Behaviour::new(
                    rendezvous::server::Config::default(),
                ),
                relay: relay::Behaviour::new(keypair.public().to_peer_id(), {
                    let mut x = relay::Config::default();
                    x.max_circuit_bytes = MAX_RELAY_CIRCUIT_BYTES;
                    x
                }),
            }
        })?
        .build();

    // Listen on all interfaces
    let listen_addr_tcp = Multiaddr::empty()
        .with(multiaddr::Protocol::from(Ipv4Addr::UNSPECIFIED))
        .with(multiaddr::Protocol::Tcp(opt.port));
    swarm.listen_on(listen_addr_tcp)?;

    let listen_addr_quic = Multiaddr::empty()
        .with(multiaddr::Protocol::from(Ipv4Addr::UNSPECIFIED))
        .with(multiaddr::Protocol::Udp(opt.port))
        .with(multiaddr::Protocol::QuicV1);
    swarm.listen_on(listen_addr_quic)?;

    let metrics = Metrics::new(&mut metric_registry);
    tokio::spawn(http_service::metrics_server(metric_registry));

    loop {
        let event = swarm.next().await;
        handle_swarm_event(
            &mut swarm,
            event.expect("Swarm stream to be infinite."),
            &metrics,
        )
        .await;
    }
}

async fn handle_swarm_event(
    swarm: &mut Swarm<Behaviour>,
    event: SwarmEvent<BehaviourEvent>,
    metrics: &Metrics,
) {
    match event {
        SwarmEvent::Behaviour(event) => {
            handle_swarm_behaviour_event(swarm, event, metrics).await;
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            info!("Listening on {address:?}");
        }
        _ => {}
    }
}

async fn handle_swarm_behaviour_event(
    swarm: &mut Swarm<Behaviour>,
    event: BehaviourEvent,
    metrics: &Metrics,
) {
    match event {
        BehaviourEvent::Autonat(event) => {
            handle_autonat_event(event);
        }
        BehaviourEvent::Identify(event) => {
            metrics.record(&event);
            info!("Identify event: {event:?}");
            if let identify::Event::Received {
                info: identify::Info { observed_addr, .. },
                ..
            } = event
            {
                info!("Adding external address: {observed_addr:?}");
                swarm.add_external_address(observed_addr);
            }
        }
        BehaviourEvent::Kad(event) => {
            metrics.record(&event);
            info!("Kad event: {event:?}");
        }
        BehaviourEvent::Relay(event) => {
            metrics.record(&event);
            info!("Relay event: {event:?}");
        }
        BehaviourEvent::Rendezvous(event) => {
            info!("Rendezvous event: {event:?}");
        }
        _ => {}
    }
}

fn handle_autonat_event(event: autonat_v2::Event) {
    match event {
        autonat_v2::Event::Client {
            tested_addr,
            bytes_sent,
            server,
            result,
        } => match result {
            autonat_v2::TestResult::Reachable { addr } => {
                info!(
                    %tested_addr,
                    %bytes_sent,
                    %server,
                    confirmed_addr = %addr,
                    "AutoNAT v2 client: address confirmed reachable"
                );
            }
            autonat_v2::TestResult::Failed { error } => {
                info!(
                    %tested_addr,
                    %bytes_sent,
                    %server,
                    %error,
                    "AutoNAT v2 client: address test failed"
                );
            }
        },
        autonat_v2::Event::Server {
            all_addrs,
            tested_addr,
            client,
            data_amount,
            result,
        } => match result {
            autonat_v2::TestResult::Reachable { addr } => {
                info!(
                    ?all_addrs,
                    %tested_addr,
                    %client,
                    %data_amount,
                    confirmed_addr = %addr,
                    "AutoNAT v2 server: served dial-back, client is reachable"
                );
            }
            autonat_v2::TestResult::Failed { error } => {
                info!(
                    ?all_addrs,
                    %tested_addr,
                    %client,
                    %data_amount,
                    %error,
                    "AutoNAT v2 server: dial-back failed"
                );
            }
        },
        autonat_v2::Event::ModeChanged { old_mode, new_mode } => {
            info!(?old_mode, ?new_mode, "AutoNAT v2 mode changed");
        }
        autonat_v2::Event::PeerHasServerSupport { peer_id } => {
            info!(%peer_id, "AutoNAT v2: discovered peer has server support");
        }
    }
}
