use std::collections::HashSet;
use std::str::FromStr;

use clap::Parser;
use clap::ValueEnum;
use futures_util::{SinkExt, StreamExt};
use libp2p::gossipsub;
use libp2p::identity;
use libp2p::PeerId;
use multiaddr::Multiaddr;
use tokio::io::AsyncBufReadExt;
use tracing::{debug, error, info, warn};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

mod network;
mod store;
mod types;

#[derive(Debug, Parser)]
#[clap(name = "Chat example")]
struct Opt {
    /// The mode (interactive, echo).
    #[clap(long, value_enum)]
    mode: Mode,

    /// The port used to listen on all interfaces
    #[clap(long)]
    port: u16,

    /// Fixed value to generate deterministic peer id.
    #[clap(long)]
    secret_key_seed: u8,

    /// The listening address of a relay server to connect to.
    #[clap(long)]
    boot_nodes: Vec<Multiaddr>,

    /// The listening address of a relay server to connect to.
    #[clap(long, default_value = "/calimero/devnet/examples/chat")]
    rendezvous_namespace: String,

    /// Optional list of peer addresses to dial immediately after network bootstrap.
    #[clap(long)]
    dial_peer_addrs: Option<Vec<Multiaddr>>,

    /// Optional list of gossip topic names to subscribe immediately after network bootstrap.
    #[clap(long)]
    gossip_topic_names: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Parser, ValueEnum)]
enum Mode {
    Interactive,
    Echo,
}

impl Mode {
    fn is_interactive(&self) -> bool {
        matches!(self, Mode::Interactive)
    }
}

struct Node {
    mode: Mode,
    store: store::Store,
    network_client: network::client::NetworkClient,
    pending_catchups: HashSet<gossipsub::TopicHash>,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::registry()
        // "info,chat_example=debug,libp2p_mdns=warn,{}",
        .with(EnvFilter::builder().parse(format!(
            "info,libp2p_mdns=warn,{}",
            std::env::var("RUST_LOG").unwrap_or_default()
        ))?)
        .with(tracing_subscriber::fmt::layer())
        .init();

    let opt = Opt::parse();

    let keypair = generate_ed25519(opt.secret_key_seed);

    let store = store::Store::default();

    let (network_client, mut network_events) = network::run(
        keypair.clone(),
        opt.port,
        opt.boot_nodes,
        libp2p::rendezvous::Namespace::new(opt.rendezvous_namespace)?,
    )
    .await?;

    let mut node = Node::new(opt.mode.clone(), store.clone(), network_client.clone());

    node.boot(opt.dial_peer_addrs, opt.gossip_topic_names)
        .await?;

    let peer_id = keypair.public().to_peer_id();
    match opt.mode {
        Mode::Interactive => {
            let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();

            loop {
                tokio::select! {
                    event = network_events.recv() => {
                        let Some(event) = event else {
                            break;
                        };
                        node.handle_network_event(event, peer_id).await?;
                    }
                    line = stdin.next_line() => {
                        if let Some(line) = line? {
                            node.handle_line(line).await?;
                        }
                    }
                }
            }
        }
        Mode::Echo => {
            while let Some(event) = network_events.recv().await {
                node.handle_network_event(event, peer_id).await?;
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

const LINE_START: &str = ">>>>>>>>>> ";

impl Node {
    fn new(
        mode: Mode,
        store: store::Store,
        network_client: network::client::NetworkClient,
    ) -> Self {
        Self {
            mode,
            store,
            network_client,
            pending_catchups: Default::default(),
        }
    }

    async fn boot(
        &mut self,
        dial_peer_addrs: Option<Vec<Multiaddr>>,
        gossip_topic_names: Option<Vec<String>>,
    ) -> eyre::Result<()> {
        if let Some(peer_addrs) = dial_peer_addrs {
            for addr in peer_addrs {
                info!("Dialing peer: {}", addr);
                self.network_client.dial(addr).await?;
            }
        }

        if let Some(topic_names) = gossip_topic_names {
            for topic_name in topic_names {
                info!("Subscribing to topic: {}", topic_name);
                let topic = gossipsub::IdentTopic::new(topic_name);

                if self.mode.is_interactive() {
                    self.pending_catchups.insert(topic.hash().clone());
                }

                self.pending_catchups.insert(topic.hash().clone());
                self.network_client.subscribe(topic.clone()).await?;
            }
        }
        Ok(())
    }

    async fn handle_network_event(
        &mut self,
        event: network::types::NetworkEvent,
        local_peer_id: PeerId,
    ) -> eyre::Result<()> {
        match event {
            network::types::NetworkEvent::Subscribed { peer_id, topic } => {
                info!("Peer '{}' subscribed to topic: {}", peer_id, topic);

                if self.pending_catchups.contains(&topic) {
                    if let Err(err) = self.perform_catchup(topic.clone(), peer_id).await {
                        error!(%err, "Failed to perform catchup");
                    } else {
                        self.pending_catchups.remove(&topic);
                    }
                }
            }
            network::types::NetworkEvent::Message { message, .. } => {
                if let Err(err) = self.handle_message(local_peer_id, message).await {
                    error!(%err, "Failed to handle message");
                }
            }
            network::types::NetworkEvent::ListeningOn { address, .. } => {
                info!("Listening on: {}", address);
            }
            network::types::NetworkEvent::StreamOpened { peer_id, stream } => {
                info!("Stream opened from peer: {}", peer_id);
                if let Err(err) = self.handle_stream(stream).await {
                    error!(%err, "Failed to handle stream");
                }

                info!("Stream closed from peer: {:?}", peer_id);
            }
        }
        Ok(())
    }

    async fn handle_message(
        &mut self,
        peer_id: PeerId,
        message: gossipsub::Message,
    ) -> eyre::Result<()> {
        let text = String::from_utf8_lossy(&message.data);
        println!(
            "{LINE_START} Received message: {:?}, from: {:?}",
            text, message.source
        );

        self.store
            .add_message(
                types::ApplicationId::from(message.topic.clone().into_string()),
                types::ChatMessage::new(message.source, message.data.clone()),
            )
            .await;

        if self.mode.is_interactive() {
            return Ok(());
        }

        if text.starts_with("echo") {
            debug!("Ignoring echo message");
            return Ok(());
        }
        let text = format!("echo ({}): '{}'", peer_id, text);

        self.network_client
            .publish(message.topic, text.into_bytes())
            .await?;
        Ok(())
    }

    async fn handle_stream(&mut self, mut stream: network::stream::Stream) -> eyre::Result<()> {
        let application_id = match stream.next().await {
            Some(message) => match serde_json::from_slice(&message.data)? {
                types::CatchupStreamMessage::Request(req) => {
                    types::ApplicationId::from(req.application_id)
                }
                message => {
                    eyre::bail!("Unexpected message: {:?}", message)
                }
            },
            None => {
                eyre::bail!("Stream closed unexpectedly")
            }
        };

        let mut iter = self.store.batch_stream(application_id, 3);
        while let Some(messages) = iter.next().await {
            info!("Sending batch: {:?}", messages);
            let response = serde_json::to_vec(&types::CatchupStreamMessage::Response(
                types::CatchupResponse { messages },
            ))?;

            stream
                .send(network::stream::Message { data: response })
                .await?;
        }

        Ok(())
    }

    async fn perform_catchup(
        &mut self,
        topic: gossipsub::TopicHash,
        choosen_peer: PeerId,
    ) -> eyre::Result<Option<()>> {
        let mut stream = self.network_client.open_stream(choosen_peer).await?;
        info!("Opened stream to peer: {:?}", choosen_peer);

        let request = serde_json::to_vec(&types::CatchupStreamMessage::Request(
            types::CatchupRequest {
                application_id: topic.clone().into_string(),
            },
        ))?;

        stream
            .send(network::stream::Message { data: request })
            .await?;

        info!("Sent catchup request to peer: {:?}", choosen_peer);

        while let Some(message) = stream.next().await {
            match serde_json::from_slice(&message.data)? {
                types::CatchupStreamMessage::Response(response) => {
                    for message in response.messages {
                        let text = String::from_utf8_lossy(&message.data);
                        println!(
                            "{LINE_START} Received cacthup message: {:?}, original from: {:?}",
                            text, message.source
                        );

                        self.store
                            .add_message(
                                types::ApplicationId::from(topic.clone().into_string()),
                                message,
                            )
                            .await;
                    }
                }
                event => {
                    warn!(?event, "Unexpected event");
                }
            };
        }

        info!("Closed stream to peer: {:?}", choosen_peer);
        return Ok(Some(()));
    }

    async fn handle_line(&mut self, line: String) -> eyre::Result<()> {
        let (command, args) = match line.split_once(' ') {
            Some((method, payload)) => (method, Some(payload)),
            None => (line.as_str(), None),
        };

        match command {
            "dial" => {
                let args = match args {
                    Some(args) => args,
                    None => {
                        println!("{LINE_START} Usage: dial <multiaddr>");
                        return Ok(());
                    }
                };

                let addr = match Multiaddr::from_str(args) {
                    Ok(addr) => addr,
                    Err(err) => {
                        println!("{LINE_START} Failed to parse MultiAddr: {:?}", err);
                        return Ok(());
                    }
                };

                info!("{LINE_START} Dialing {:?}", addr);

                match self.network_client.dial(addr).await {
                    Ok(_) => {
                        println!("{LINE_START} Peer dialed");
                    }
                    Err(err) => {
                        println!("{LINE_START} Failed to dial peer: {:?}", err);
                    }
                };
            }
            "subscribe" => {
                let args = match args {
                    Some(args) => args,
                    None => {
                        println!("{LINE_START} Usage: subscribe <topic-name>");
                        return Ok(());
                    }
                };

                let topic = gossipsub::IdentTopic::new(args.to_string());
                if self.mode.is_interactive() {
                    self.pending_catchups.insert(topic.hash().clone());
                }

                match self.network_client.subscribe(topic).await {
                    Ok(_) => {
                        println!("{LINE_START} Subscribed to topic");
                    }
                    Err(err) => {
                        println!("{LINE_START} Failed to subscribe to topic: {:?}", err);
                    }
                };
            }
            "unsubscribe" => {
                let args = match args {
                    Some(args) => args,
                    None => {
                        println!("{LINE_START} Usage: unsubscribe <topic-name>");
                        return Ok(());
                    }
                };

                let topic = gossipsub::IdentTopic::new(args.to_string());
                match self.network_client.unsubscribe(topic).await {
                    Ok(_) => {
                        println!("{LINE_START} Unsubscribed from topic");
                    }
                    Err(err) => {
                        println!("{LINE_START} Failed to unsubscribe from topic: {:?}", err);
                    }
                };
            }
            "publish" => {
                let args = match args {
                    Some(args) => args,
                    None => {
                        println!("{LINE_START} Usage: message <topic-name> <message>");
                        return Ok(());
                    }
                };

                let mut args_iter = args.split_whitespace();
                let topic_name = match args_iter.next() {
                    Some(topic) => topic,
                    None => {
                        println!("{LINE_START} Usage: message <topic-name> <message>");
                        return Ok(());
                    }
                };

                let message_data = match args_iter.next() {
                    Some(data) => data,
                    None => {
                        println!("{LINE_START} Usage: message <topic-name> <message>");
                        return Ok(());
                    }
                };

                let topic = gossipsub::IdentTopic::new(topic_name.to_string());
                match self
                    .network_client
                    .publish(topic.hash(), message_data.as_bytes().to_vec())
                    .await
                {
                    Ok(_) => {
                        println!("{LINE_START} Message published successfully");
                    }
                    Err(err) => {
                        println!("{LINE_START} Failed to publish message: {:?}", err);
                    }
                };
            }
            "peers" => {
                let peer_info = self.network_client.peer_info().await;
                println!("{LINE_START} Peer info: {:?}", peer_info);
            }
            "mesh-peers" => {
                let args = match args {
                    Some(args) => args,
                    None => {
                        println!("{LINE_START} Usage: mesh-peers <topic-name>");
                        return Ok(());
                    }
                };

                let topic = gossipsub::IdentTopic::new(args.to_string());
                let mesh_peer_info = self.network_client.mesh_peer_info(topic.hash()).await;
                println!("{LINE_START} Mesh peer info: {:?}", mesh_peer_info);
            }
            _ => println!("{LINE_START} Unknown command"),
        }

        Ok(())
    }
}
