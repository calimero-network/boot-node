use std::collections::HashSet;

use eyre::ContextCompat;
use libp2p::PeerId;
use multiaddr::Protocol;
use tracing::{debug, error, info};

pub(crate) mod model;
use super::EventLoop;

impl EventLoop {
    // Handles rendezvous discoveries for all rendezvous peers.
    // If rendezvous peer is not connected, it will be dialed which will trigger the registration on during identify exchange.
    pub(crate) async fn handle_rendezvous_discoveries(&mut self) {
        for peer_id in self
            .network_state
            .get_rendezvous_peer_ids()
            .collect::<Vec<_>>()
        {
            let peer_info = match self.network_state.get_peer_info(&peer_id) {
                Some(info) => info,
                None => {
                    error!(%peer_id, "Failed to lookup peer info");
                    continue;
                }
            };

            if !self.swarm.is_connected(&peer_id) {
                for addr in peer_info.addrs().cloned() {
                    if let Err(err) = self.swarm.dial(addr) {
                        error!(%err, "Failed to dial relay peer");
                    }
                }
            } else {
                if let Err(err) = self.perform_rendezvous_discovery(&peer_id) {
                    error!(%err, "Failed to handle rendezvous discover");
                }
            }
        }
    }

    // Performs rendezvous discovery against the remote rendezvous peer if it's time to do so.
    // This function expectes that the relay peer is already connected.
    pub(crate) fn perform_rendezvous_discovery(
        &mut self,
        rendezvous_peer: &PeerId,
    ) -> eyre::Result<()> {
        let peer_info = self
            .network_state
            .get_peer_info(rendezvous_peer)
            .wrap_err("Failed to get peer info {}")?;

        if peer_info.is_rendezvous_discovery_time() {
            self.swarm.behaviour_mut().rendezvous.discover(
                Some(self.rendezvous_namespace.clone()),
                peer_info.rendezvous_cookie().cloned(),
                None,
                *rendezvous_peer,
            );
        }

        Ok(())
    }

    // Broadcasts rendezvous registrations to all rendezvous peers if there are pending address changes.
    // If rendezvous peer is not connected, it will be dialed which will trigger the registration on during identify exchange.
    pub(crate) fn broadcast_rendezvous_registrations(&mut self) -> eyre::Result<()> {
        if !self.network_state.pending_addr_changes() {
            return Ok(());
        }

        for peer_id in self
            .network_state
            .get_rendezvous_peer_ids()
            .collect::<Vec<_>>()
        {
            let peer_info = match self.network_state.get_peer_info(&peer_id) {
                Some(info) => info,
                None => {
                    error!(%peer_id, "Failed to lookup peer info");
                    continue;
                }
            };

            if !self.swarm.is_connected(&peer_id) {
                for addr in peer_info.addrs().cloned() {
                    if let Err(err) = self.swarm.dial(addr) {
                        error!(%err, "Failed to dial relay peer");
                    }
                }
            } else {
                if let Err(err) = self.update_rendezvous_registration(&peer_id) {
                    error!(%err, "Failed to handle rendezvous discover");
                }
            }
        }

        self.network_state.clear_pending_addr_changes();

        Ok(())
    }

    // Updates rendezvous registration on the remote rendezvous peer.
    // If there are no external addresses for the node, the registration is considered successful.
    // This function expectes that the relay peer is already connected.
    pub(crate) fn update_rendezvous_registration(
        &mut self,
        rendezvous_peer: &PeerId,
    ) -> eyre::Result<()> {
        if let Err(err) = self.swarm.behaviour_mut().rendezvous.register(
            self.rendezvous_namespace.clone(),
            *rendezvous_peer,
            None,
        ) {
            match err {
                libp2p::rendezvous::client::RegisterError::NoExternalAddresses => {}
                err => eyre::bail!(err),
            }
        }

        debug!(
            %rendezvous_peer, rendezvous_namespace=%(self.rendezvous_namespace),
            "Sent register request to rendezvous node"
        );
        Ok(())
    }

    // Creates relay reservation if node doesn't have a relayed address on the relay peer.
    // This function expectes that the relay peer is already connected.
    pub(crate) fn create_relay_reservation(&mut self, relay_peer: &PeerId) -> eyre::Result<()> {
        let peer_info = self
            .network_state
            .get_peer_info(relay_peer)
            .wrap_err("Failed to get peer info")?;

        let external_addrs = self
            .swarm
            .external_addresses()
            .filter(|addr| addr.iter().any(|p| matches!(p, Protocol::P2pCircuit)))
            .map(|addr| addr.clone())
            .collect::<HashSet<_>>();

        let preferred_addr = peer_info
            .get_preferred_addr()
            .wrap_err("Failed to get preferred addr for relay peer")?;

        let relayed_addr = match preferred_addr
            .clone()
            .with(multiaddr::Protocol::P2pCircuit)
            .with_p2p(self.swarm.local_peer_id().clone())
        {
            Ok(addr) => addr,
            Err(err) => {
                eyre::bail!("Failed to construct relayed addr for relay peer: {:?}", err)
            }
        };
        let is_relay_reservation_required = !(matches!(
            peer_info.relay_reservation_status(),
            Some(model::RelayReservationStatus::Requested)
        ) || external_addrs.contains(&relayed_addr));

        debug!(
            ?peer_info,
            ?external_addrs,
            %is_relay_reservation_required,
            "Checking if relay reservation is required"
        );

        if !is_relay_reservation_required {
            return Ok(());
        }

        self.swarm.listen_on(relayed_addr)?;
        self.network_state.update_relay_reservation_status(
            &relay_peer,
            model::RelayReservationStatus::Requested,
        )?;

        Ok(())
    }
}
