# Chat 
This examples show cases how to manually dial (connect to) either local peer or remote peer has a reservation on relay-server. 

## Run local only
This examples shows how to run two sessions locally and connect sessions by manually dialing local peer.

Run first chat session in echo mode.
```
cargo run -p chat-example -- --mode echo --port 4002 --secret-key-seed 102 --gossip-topic-names calimero-network/examples/chat/v0.0.1 --relay-address /ip4/3.71.239.80/udp/4001/quic-v1/p2p/12D3KooWAgFah4EZtWnMMGMUddGdJpb5cq2NubNCAD2jA5AZgbXF
```

Run second chat session in interactive mode with local peer dial.
```
cargo run -p chat-example -- --mode interactive --port 4003 --secret-key-seed 103 --gossip-topic-names calimero-network/examples/chat/v0.0.1 --dial-peer-addrs /ip4/127.0.0.1/udp/4002/quic-v1/p2p/12D3KooWMpeKAbMK4BTPsQY3rG7XwtdstseHGcq7kffY8LToYYKK --relay-address /ip4/3.71.239.80/udp/4001/quic-v1/p2p/12D3KooWAgFah4EZtWnMMGMUddGdJpb5cq2NubNCAD2jA5AZgbXF
```

In the interactive session publish new message manually:
```
publish calimero-network/examples/chat/v0.0.1 ola
```

## Run locally with remote peer dial in
This examples shows how to run two sessions locally and connect sessions manually by dialing private remote peer from each session. For the gossip message to pass from one local session to second local session it needs to go "the long way" around (local -> remote -> local).

Run first chat session in interactive mode with remote peer dial.
```
```

Run second chat session in interactive mode with remote peer dial.
```
```

In any interactive session publish new message manually:
```
publish calimero-network/examples/chat/v0.0.1 ola
```

## Debugging and known issues
- If multiple people are running the same example, some will fail to get reservation on relay server because the same PeerId already exists.
  - Fix: change `secret-key-seed` to something else

