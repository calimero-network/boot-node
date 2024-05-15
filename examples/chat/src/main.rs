use std::str::FromStr;

use clap::Parser;
use libp2p::gossipsub;
use libp2p::identity;
use multiaddr::Multiaddr;
use tokio::io::AsyncBufReadExt;
use tracing::{error, info};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

mod network;

#[derive(Debug, Parser)]
#[clap(name = "Chat example")]
struct Opt {
    /// The mode (interactive, echo).
    #[clap(long)]
    mode: Mode,

    /// The port used to listen on all interfaces
    #[clap(long)]
    port: u16,

    /// Fixed value to generate deterministic peer id.
    #[clap(long)]
    secret_key_seed: u8,

    /// The listening address of a relay server to connect to.
    #[clap(long)]
    relay_address: Multiaddr,

    /// Optional list of peer addresses to dial immediately after network bootstrap.
    #[clap(long)]
    dial_peer_addrs: Option<Vec<Multiaddr>>,

    /// Optional list of gossip topic names to subscribe immediately after network bootstrap.
    #[clap(long)]
    gossip_topic_names: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Parser)]
enum Mode {
    Interactive,
    Echo,
}

impl FromStr for Mode {
    type Err = String;
    fn from_str(mode: &str) -> Result<Self, Self::Err> {
        match mode {
            "interactive" => Ok(Mode::Interactive),
            "echo" => Ok(Mode::Echo),
            _ => Err("Expected either 'dial' or 'listen'".to_string()),
        }
    }
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::registry()
        // "info,chat_example=debug,{}",
        .with(EnvFilter::builder().parse(format!(
            "info,{}",
            std::env::var("RUST_LOG").unwrap_or_default()
        ))?)
        .with(tracing_subscriber::fmt::layer())
        .init();

    let opt = Opt::parse();

    let keypair = generate_ed25519(opt.secret_key_seed);

    let (network_client, mut network_events) =
        network::run(keypair, opt.port, opt.relay_address.clone()).await?;

    if let Some(peer_addrs) = opt.dial_peer_addrs {
        for addr in peer_addrs {
            network_client.dial(addr).await?;
        }
    }

    if let Some(topic_names) = opt.gossip_topic_names {
        for topic_name in topic_names {
            let topic = gossipsub::IdentTopic::new(topic_name);
            network_client.subscribe(topic).await?;
        }
    }

    match opt.mode {
        Mode::Interactive => {
            let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();

            loop {
                tokio::select! {
                    event = network_events.recv() => {
                        let Some(event) = event else {
                            break;
                        };
                        handle_network_event(Mode::Interactive, network_client.clone(), event).await?;
                    }
                    line = stdin.next_line() => {
                        if let Some(line) = line? {
                            handle_line(network_client.clone(), line).await?;
                        }
                    }
                }
            }
        }
        Mode::Echo => {
            while let Some(event) = network_events.recv().await {
                handle_network_event(Mode::Echo, network_client.clone(), event).await?;
            }
        }
    }

    Ok(())
}

fn generate_ed25519(secret_key_seed: u8) -> identity::Keypair {
    let mut bytes = [0u8; 32];
    bytes[0] = secret_key_seed;

    identity::Keypair::ed25519_from_bytes(bytes).expect("only errors on wrong length")
}

async fn handle_network_event(
    mode: Mode,
    network_client: network::client::NetworkClient,
    event: network::types::NetworkEvent,
) -> eyre::Result<()> {
    match event {
        network::types::NetworkEvent::IdentifySent { peer_id } => {
            info!("Identify sent to {:?}", peer_id);
        }
        network::types::NetworkEvent::IdentifyReceived {
            peer_id,
            observed_addr,
        } => {
            info!(
                "Identify received from {:?} at {:?}",
                peer_id, observed_addr
            );
        }
        network::types::NetworkEvent::Message { id, message } => {
            let text = String::from_utf8_lossy(&message.data);
            info!("Message from {:?}: {:?}", id, text);

            match mode {
                Mode::Echo => {
                    let text = format!("Echo, original: '{}'", text);

                    match network_client
                        .publish(message.topic, text.into_bytes())
                        .await
                    {
                        Ok(_) => info!("Echoed message back"),
                        Err(err) => error!(%err, "Failed to echo message back"),
                    };
                }
                _ => {}
            }
        }
        network::types::NetworkEvent::ListeningOn { address, .. } => {
            info!("Listening on: {}", address);
        }
        event => {
            info!("Unhandled event: {:?}", event);
        }
    }
    Ok(())
}

async fn handle_line(
    network_client: network::client::NetworkClient,
    line: String,
) -> eyre::Result<()> {
    let (command, args) = match line.split_once(' ') {
        Some((method, payload)) => (method, Some(payload)),
        None => (line.as_str(), None),
    };

    match command {
        "dial" => {
            let args = match args {
                Some(args) => args,
                None => {
                    println!("Usage: dial <multiaddr>");
                    return Ok(());
                }
            };

            let addr = match Multiaddr::from_str(args) {
                Ok(addr) => addr,
                Err(err) => {
                    println!("Failed to parse MultiAddr: {:?}", err);
                    return Ok(());
                }
            };

            info!("Dialing {:?}", addr);

            match network_client.dial(addr).await {
                Ok(_) => {
                    println!("Peer dialed");
                }
                Err(err) => {
                    println!("Failed to parse peer id: {:?}", err);
                }
            };
        }
        "subscribe" => {
            let args = match args {
                Some(args) => args,
                None => {
                    println!("Usage: subscribe <topic-name>");
                    return Ok(());
                }
            };

            let topic = gossipsub::IdentTopic::new(args.to_string());
            match network_client.subscribe(topic).await {
                Ok(_) => {
                    println!("Peer dialed");
                }
                Err(err) => {
                    println!("Failed to parse peer id: {:?}", err);
                }
            };
        }
        "unsubscribe" => {
            let args = match args {
                Some(args) => args,
                None => {
                    println!("Usage: unsubscribe <topic-name>");
                    return Ok(());
                }
            };

            let topic = gossipsub::IdentTopic::new(args.to_string());
            match network_client.unsubscribe(topic).await {
                Ok(_) => {
                    println!("Peer dialed");
                }
                Err(err) => {
                    println!("Failed to parse peer id: {:?}", err);
                }
            };
        }
        "publish" => {
            let args = match args {
                Some(args) => args,
                None => {
                    println!("Usage: message <topic-name> <message>");
                    return Ok(());
                }
            };

            // Extracting topic name and message data from args
            let mut args_iter = args.split_whitespace();
            let topic_name = match args_iter.next() {
                Some(topic) => topic,
                None => {
                    println!("Usage: message <topic-name> <message>");
                    return Ok(());
                }
            };

            let message_data = match args_iter.next() {
                Some(data) => data,
                None => {
                    println!("Usage: message <topic-name> <message>");
                    return Ok(());
                }
            };

            let topic = gossipsub::IdentTopic::new(topic_name.to_string());

            // Publishing the message
            match network_client
                .publish(topic.hash(), message_data.as_bytes().to_vec())
                .await
            {
                Ok(_) => {
                    println!("Message published successfully");
                }
                Err(err) => {
                    println!("Failed to publish message: {:?}", err);
                }
            };
        }
        "peers" => {
            let peer_info = network_client.peer_info().await;
            info!("Peer info: {:?}", peer_info);
        }
        "mesh-peers" => {
            let args = match args {
                Some(args) => args,
                None => {
                    println!("Usage: mesh-peers <topic-name>");
                    return Ok(());
                }
            };

            let topic = gossipsub::IdentTopic::new(args.to_string());
            let mesh_peer_info = network_client.mesh_peer_info(topic.hash()).await;
            info!("Mesh peer info: {:?}", mesh_peer_info);
        }
        _ => info!("Unknown command"),
    }

    Ok(())
}
