use std::str::FromStr;

use clap::Parser;
use libp2p::gossipsub;
use libp2p::identity;
use multiaddr::Multiaddr;
use tokio::io::AsyncBufReadExt;
use tracing::info;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

mod network;

#[derive(Debug, Parser)]
#[clap(name = "DCUtR client example")]
struct Opt {
    /// Fixed value to generate deterministic peer id.
    #[clap(long)]
    secret_key_seed: u8,

    /// The listening address
    #[clap(long)]
    relay_address: Multiaddr,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::registry()
        // "info,chat_example::network=debug,{}",
        .with(EnvFilter::builder().parse(format!(
            "info,chat_example::network=debug,{}",
            std::env::var("RUST_LOG").unwrap_or_default()
        ))?)
        .with(tracing_subscriber::fmt::layer())
        .init();

    let opt = Opt::parse();

    let keypair = generate_ed25519(opt.secret_key_seed);

    let (network_client, mut network_events) =
        network::run(keypair, opt.relay_address.clone()).await?;

    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();

    loop {
        tokio::select! {
            event = network_events.recv() => {
                let Some(event) = event else {
                    break;
                };
                handle_event(network_client.clone(), event).await?;
            }
            line = stdin.next_line() => {
                if let Some(line) = line? {
                    handle_line(network_client.clone(), line).await?;
                }
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

async fn handle_event(
    _: network::client::NetworkClient,
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
