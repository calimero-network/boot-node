# Multi-stage Dockerfile for the calimero-network boot-node.
#
# Building from the binary that was just produced by the release
# CI (release.yml). The binary is platform-specific, so the
# release workflow does the cross-builds and passes the right
# artifact in via `--build-arg BOOT_NODE_BINARY=...`. The image
# itself is multi-arch via buildx (linux/amd64 + linux/arm64);
# each platform variant copies the matching binary from the
# release artifacts.
#
# `debian:bookworm-slim` keeps the runtime image small (~80MB)
# without going to `scratch` — boot-node is a glibc binary built
# via standard `cargo build`, and `scratch` would force a musl
# rebuild. The ca-certificates package is required for any
# rendezvous-bootstrap-peer that uses TLS.

FROM debian:bookworm-slim

# The binary is copied in from the release artifacts. The release
# workflow runs `cargo build --release` (x86_64) + `cross build`
# (aarch64), then renames the outputs to predictable names that
# match this ARG default. buildx picks the right one for the
# target platform via TARGETARCH.
ARG TARGETARCH

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy the platform-matching binary. The release workflow stages
# both architectures under `artifacts/<docker-arch>/boot-node`
# (amd64 ← x86_64-unknown-linux build, arm64 ← aarch64-unknown-
# linux build) so this COPY can use TARGETARCH directly without
# any shell-side mapping inside the Dockerfile.
COPY artifacts/${TARGETARCH}/boot-node /usr/local/bin/boot-node
RUN chmod +x /usr/local/bin/boot-node

# 4001 is the default libp2p listen port (the boot-node binary's
# `--port` flag defaults to it too). Single port covers TCP and
# QUIC — libp2p listens on both with the same number.
EXPOSE 4001/tcp 4001/udp

# `--dev` generates an ephemeral keypair on each startup, so the
# container is usable with no extra setup. Operators that need a
# stable peer id should mount a keypair and override with
# `--private-key /path`.
CMD ["boot-node", "--dev"]
