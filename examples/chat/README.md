# Chat 
This examples show cases how to build chat application with DCUtR, mDNS, Relay, Rendezvous and GossipSub protocols.
- Local node gets reservation on the relay.
- Local node advertises relayed address at the Rendezvous node.
- Local nodes discovers other local nodes via mDNS discovery and attempts direct connection.
- Local nodes discovers other remote nodes via Rendezvous discovery and attempts hole punched connectioned.

## Run
Run first chat session in interactive mode with remote peer dial.
```
cargo run -p chat-example -- --mode interactive --port 4002 --secret-key-seed 102 --gossip-topic-names calimero-network/examples/chat/v0.0.2 --boot-nodes /ip4/35.156.78.13/udp/4001/quic-v1/p2p/12D3KooWRnt7EmBwrNALhAXAgM151MdH7Ka9tvYS91ZUqnqwpjVg
```

Run second chat session in interactive mode with remote peer dial.
```
cargo run -p chat-example -- --mode interactive --port 4003 --secret-key-seed 103 --gossip-topic-names calimero-network/examples/chat/v0.0.2 --boot-nodes /ip4/35.156.78.13/udp/4001/quic-v1/p2p/12D3KooWRnt7EmBwrNALhAXAgM151MdH7Ka9tvYS91ZUqnqwpjVg
```

In any interactive session publish new message manually:
```
publish calimero-network/examples/chat/v0.0.2 ola
```

## Debugging and known issues
- If multiple people are running the same example, some will fail to get reservation on relay server because the same PeerId already exists.
  - Fix: change `secret-key-seed` to something else

