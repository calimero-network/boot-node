use std::str::FromStr;
use std::{error::Error, time::Duration};

use clap::Parser;
use libp2p::futures::prelude::*;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{dcutr, identify, identity, noise, ping, relay, yamux, Multiaddr, PeerId};
use multiaddr::Protocol;
use tracing::info;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[clap(name = "DCUtR client example")]
struct Opt {
    /// The mode (client-listen, client-dial).
    #[clap(long)]
    mode: Mode,

    /// Fixed value to generate deterministic peer id.
    #[clap(long)]
    secret_key_seed: u8,

    /// The listening address
    #[clap(long)]
    relay_address: Multiaddr,

    /// Peer ID of the remote peer to hole punch to.
    #[clap(long)]
    remote_peer_id: Option<PeerId>,
}

#[derive(Clone, Debug, PartialEq, Parser)]
enum Mode {
    Dial,
    Listen,
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(mode: &str) -> Result<Self, Self::Err> {
        match mode {
            "dial" => Ok(Mode::Dial),
            "listen" => Ok(Mode::Listen),
            _ => Err("Expected either 'dial' or 'listen'".to_string()),
        }
    }
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay_client: relay::client::Behaviour,
    ping: ping::Behaviour,
    identify: identify::Behaviour,
    dcutr: dcutr::Behaviour,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::registry()
        .with(EnvFilter::builder().parse(format!(
            "info,{}",
            std::env::var("RUST_LOG").unwrap_or_default()
        ))?)
        .with(tracing_subscriber::fmt::layer())
        .init();

    let opt = Opt::parse();

    let mut swarm =
        libp2p::SwarmBuilder::with_existing_identity(generate_ed25519(opt.secret_key_seed))
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default()
                    .port_reuse(true)
                    .nodelay(true),
                (libp2p::tls::Config::new, libp2p::noise::Config::new),
                libp2p::yamux::Config::default,
            )?
            .with_quic()
            .with_dns()?
            .with_relay_client(noise::Config::new, yamux::Config::default)?
            .with_behaviour(|keypair, relay_behaviour| Behaviour {
                relay_client: relay_behaviour,
                ping: ping::Behaviour::new(ping::Config::new()),
                identify: identify::Behaviour::new(identify::Config::new(
                    "/relay-server/0.2.0".to_string(),
                    keypair.public(),
                )),
                dcutr: dcutr::Behaviour::new(keypair.public().to_peer_id()),
            })?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

    swarm
        .listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    swarm
        .listen_on("/ip4/0.0.0.0/tcp/0".parse().unwrap())
        .unwrap();

    // Reference: https://github.com/libp2p/rust-libp2p/blob/60fd566a955a33c42a6ab6eefc1f0fedef9f8b83/examples/dcutr/src/main.rs#L118
    loop {
        tokio::select! {
            Some(event) = swarm.next() => {
                match event {
                    SwarmEvent::NewListenAddr { address, .. } => {
                        info!(%address, "Listening on address");
                    }
                    event => panic!("{event:?}"),
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
    swarm.dial(opt.relay_address.clone()).unwrap();

    let mut learned_observed_addr = false;
    let mut told_relay_observed_addr = false;
    loop {
        match swarm.next().await.unwrap() {
            SwarmEvent::NewListenAddr { .. } => {}
            SwarmEvent::Dialing { .. } => {}
            SwarmEvent::ConnectionEstablished { .. } => {}
            SwarmEvent::Behaviour(BehaviourEvent::Ping(_)) => {}
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Sent { .. })) => {
                info!("Told relay its public address");
                told_relay_observed_addr = true;
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                info: identify::Info { observed_addr, .. },
                ..
            })) => {
                info!(address=%observed_addr, "Relay told us our observed address");
                learned_observed_addr = true;
            }
            event => panic!("{event:?}"),
        }

        if learned_observed_addr && told_relay_observed_addr {
            break;
        }
    }

    match opt.mode {
        Mode::Dial => {
            swarm
                .dial(
                    opt.relay_address
                        .with(multiaddr::Protocol::P2pCircuit)
                        .with(Protocol::P2p(opt.remote_peer_id.unwrap())),
                )
                .unwrap();
        }
        Mode::Listen => {
            swarm
                .listen_on(opt.relay_address.with(Protocol::P2pCircuit))
                .unwrap();
        }
    }

    loop {
        match swarm.next().await.unwrap() {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(%address, "Listening on address");
            }
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                relay::client::Event::ReservationReqAccepted { .. },
            )) => {
                assert!(opt.mode == Mode::Listen);
                info!("Relay accepted our reservation request");
            }
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(event)) => {
                info!(?event)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(event)) => {
                info!(?event)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Identify(event)) => {
                info!(?event)
            }
            SwarmEvent::Behaviour(BehaviourEvent::Ping(event)) => {
                info!(?event)
            }
            SwarmEvent::ConnectionEstablished {
                peer_id, endpoint, ..
            } => {
                info!(peer=%peer_id, ?endpoint, "Established new connection");
            }
            SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                info!(peer=?peer_id, "Outgoing connection failed: {error}");
            }
            _ => {}
        }
    }
}

fn generate_ed25519(secret_key_seed: u8) -> identity::Keypair {
    let mut bytes = [0u8; 32];
    bytes[0] = secret_key_seed;

    identity::Keypair::ed25519_from_bytes(bytes).expect("only errors on wrong length")
}
