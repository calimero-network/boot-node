use std::net::Ipv4Addr;

use clap::Parser;
use libp2p::futures::prelude::*;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, kad, ping, relay, Multiaddr, StreamProtocol, Swarm};
use tracing::info;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

const PROTOCOL_VERSION: &str = concat!("/", env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
const CALIMERO_KAD_PROTO_NAME: StreamProtocol = StreamProtocol::new("/calimero/kad/1.0.0");

#[derive(NetworkBehaviour)]
struct Behaviour {
    identify: identify::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
    ping: ping::Behaviour,
    relay: relay::Behaviour,
}

#[derive(Debug, Parser)]
#[clap(name = "calimero relay")]
struct Opt {
    /// The file with the protobuf encoded private key used to derive PeerId and sign network activity
    #[clap(long, value_name = "PRIVATE_KEY")]
    #[clap(env = "RELAY_SERVER_PRIVATE_KEY", hide_env_values = true)]
    private_key: camino::Utf8PathBuf,

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

    let mut bytes = std::fs::read(opt.private_key)?;
    let keypair = identity::Keypair::from_protobuf_encoding(&mut bytes)?;
    let peer_id = keypair.public().to_peer_id();

    info!("Peer id: {:?}", peer_id);

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            Default::default(),
            (libp2p::tls::Config::new, libp2p::noise::Config::new),
            libp2p::yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|keypair| Behaviour {
            identify: identify::Behaviour::new(identify::Config::new(
                PROTOCOL_VERSION.to_owned(),
                keypair.public(),
            )),
            kad: {
                let mut kademlia_config = kad::Config::default();
                kademlia_config.set_protocol_names(vec![CALIMERO_KAD_PROTO_NAME]);
                // Instantly remove records and provider records.
                // TODO: figure out what to do with these values, ref: https://github.com/libp2p/rust-libp2p/blob/1aa016e1c7e3976748a726eab37af44d1c5b7a6e/misc/server/src/behaviour.rs#L38
                kademlia_config.set_record_ttl(Some(std::time::Duration::from_secs(0)));
                kademlia_config.set_provider_record_ttl(Some(std::time::Duration::from_secs(0)));

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
            relay: relay::Behaviour::new(keypair.public().to_peer_id(), Default::default()),
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

    loop {
        let event = swarm.next().await;
        handle_swarm_event(&mut swarm, event.expect("Swarm stream to be infinite.")).await;
    }
}

async fn handle_swarm_event(swarm: &mut Swarm<Behaviour>, event: SwarmEvent<BehaviourEvent>) {
    match event {
        SwarmEvent::Behaviour(event) => {
            handle_swarm_behaviour_event(swarm, event).await;
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            info!("Listening on {address:?}");
        }
        _ => {}
    }
}

async fn handle_swarm_behaviour_event(swarm: &mut Swarm<Behaviour>, event: BehaviourEvent) {
    match event {
        BehaviourEvent::Identify(identify::Event::Received {
            info: identify::Info { observed_addr, .. },
            ..
        }) => {
            swarm.add_external_address(observed_addr);
        }
        BehaviourEvent::Kad(event) => {
            info!("Kad event: {event:?}");
        }
        _ => {}
    }
}
