# AutoNAT Client Example

This example demonstrates NAT detection and connectivity strategies using libp2p's AutoNAT protocol.

## How It Works

The application:
1. Detects whether your node is behind NAT/firewall (private) or publicly accessible
2. Establishes appropriate connectivity based on NAT status
3. Registers with a rendezvous point for peer discovery

### Private Node Behavior
When behind NAT (most home networks):
- Detects private NAT status through AutoNAT probes
- Creates a relay reservation with a public relay node
- Uses the relay address to register with a rendezvous point
- Other peers can connect through the relay circuit

### Public Node Behavior
When publicly accessible:
- Confirms public status with high confidence (2+ successful probes)
- Registers direct address with rendezvous point
- Other peers can connect directly without relay

## Running the Example

```
cargo run
```

For custom options:
```
cargo run -- --listen-port 4001
```

## Troubleshooting

- If you can't register with the rendezvous point, check that you have a valid relay connection
- Connection timeouts may indicate firewall issues blocking libp2p protocols