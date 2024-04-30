use std::net::Ipv4Addr;

use clap::Parser;
use libp2p::futures::prelude::*;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{identify, identity, ping, relay, Multiaddr, Swarm};
use tracing::info;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

const PROTOCOL_VERSION: &str = concat!("/", env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay: relay::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
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

    info!("Peer id: {:?}", keypair.public().to_peer_id());

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            Default::default(),
            (libp2p::tls::Config::new, libp2p::noise::Config::new),
            libp2p::yamux::Config::default,
        )?
        .with_quic()
        .with_behaviour(|key| Behaviour {
            relay: relay::Behaviour::new(key.public().to_peer_id(), Default::default()),
            ping: ping::Behaviour::new(ping::Config::new()),
            identify: identify::Behaviour::new(identify::Config::new(
                PROTOCOL_VERSION.to_owned(),
                key.public(),
            )),
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
            if let BehaviourEvent::Identify(identify::Event::Received {
                info: identify::Info { observed_addr, .. },
                ..
            }) = &event
            {
                swarm.add_external_address(observed_addr.clone());
            }

            info!("{event:?}")
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            info!("Listening on {address:?}");
        }
        _ => {}
    }
}
