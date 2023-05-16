//! Peer manager for nym integration.
use crate::NetworkGlobals;
use libp2p::{
    swarm::{
        dummy::ConnectionHandler, ConnectionClosed, ConnectionId, DialFailure, FromSwarm,
        NetworkBehaviour, NetworkBehaviourAction as NBAction, PollParameters,
    },
    Multiaddr, PeerId,
};
use std::{
    sync::Arc,
    task::{Context, Poll},
};
use types::EthSpec;

pub struct PeerManager<TSpec: EthSpec> {
    /// A collection of network constants that can be read from other threads.
    pub network_globals: Arc<NetworkGlobals<TSpec>>,

    peers: Vec<(PeerId, Multiaddr)>,

    dialed_peers: Vec<PeerId>,
}

impl<TSpec: EthSpec> PeerManager<TSpec> {
    pub fn new(network_globals: Arc<NetworkGlobals<TSpec>>) -> Self {
        Self {
            network_globals,
            peers: Vec::new(),
            dialed_peers: Vec::new(),
        }
    }
}

impl<TSpec: EthSpec> NetworkBehaviour for PeerManager<TSpec> {
    type ConnectionHandler = ConnectionHandler;
    type OutEvent = Vec<(PeerId, Multiaddr)>;

    fn new_handler(&mut self) -> Self::ConnectionHandler {
        ConnectionHandler
    }

    // Main execution loop to drive the behaviour
    fn poll(
        &mut self,
        _cx: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<NBAction<Self::OutEvent, void::Void>> {
        let peer_id = self.network_globals.local_peer_id();
        let trusted_peers = self.network_globals.trusted_peers();
        self.peers = trusted_peers
            .iter()
            .filter_map(|(id, addr)| {
                if id != &peer_id {
                    Some((id.clone(), addr.clone()))
                } else {
                    None
                }
            })
            .collect();

        let dialed_peers = self.dialed_peers.clone();
        let peers = self
            .peers
            .iter()
            .filter_map(|p| {
                if dialed_peers.contains(&p.0) {
                    None
                } else {
                    Some(p.clone())
                }
            })
            .collect::<Vec<_>>();

        if peers.is_empty() {
            Poll::Pending
        } else {
            self.dialed_peers.extend(peers.iter().map(|p| p.0.clone()));
            Poll::Ready(NBAction::GenerateEvent(peers.clone()))
        }
    }

    fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) {
        match event {
            FromSwarm::DialFailure(DialFailure { peer_id, .. }) => {
                self.dialed_peers = self
                    .dialed_peers
                    .iter()
                    .filter_map(|p| {
                        if Some(p) != peer_id.as_ref() {
                            Some(p.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
            }
            FromSwarm::ConnectionClosed(ConnectionClosed { peer_id, .. }) => {
                self.dialed_peers = self
                    .dialed_peers
                    .iter()
                    .filter_map(|p| if *p != peer_id { Some(p.clone()) } else { None })
                    .collect();
            }
            FromSwarm::ConnectionEstablished(_)
            | FromSwarm::AddressChange(_)
            | FromSwarm::ListenFailure(_)
            | FromSwarm::NewListener(_)
            | FromSwarm::NewListenAddr(_)
            | FromSwarm::ExpiredListenAddr(_)
            | FromSwarm::ListenerError(_)
            | FromSwarm::ListenerClosed(_)
            | FromSwarm::NewExternalAddr(_)
            | FromSwarm::ExpiredExternalAddr(_) => {}
        }
    }

    fn on_connection_handler_event(
        &mut self,
        _peer_id: PeerId,
        _connection_id: ConnectionId,
        _event: void::Void,
    ) {
    }
}
