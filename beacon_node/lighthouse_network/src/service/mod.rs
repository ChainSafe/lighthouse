use self::behaviour::Behaviour;
use self::gossip_cache::GossipCache;
use crate::config::{gossipsub_config, NetworkLoad};
use crate::peer_manager::{MIN_OUTBOUND_ONLY_FACTOR, PEER_EXCESS_FACTOR, PRIORITY_PEER_EXCESS};
use crate::pm::PeerManager;
use crate::rpc::*;
use crate::service::behaviour::BehaviourEvent;
pub use crate::service::behaviour::Gossipsub;
use crate::types::{
    fork_core_topics, subnet_from_topic_hash, GossipEncoding, GossipKind, GossipTopic,
    SnappyTransform,
};
use crate::EnrExt;
use crate::Eth2Enr;
use crate::{error, metrics, Enr, NetworkGlobals, PubsubMessage, TopicHash};
use api_types::{PeerRequestId, Request, RequestId, Response};
use futures::stream::StreamExt;
use gossipsub_scoring_parameters::{lighthouse_gossip_thresholds, PeerScoreSettings};
use libp2p::bandwidth::BandwidthSinks;
use libp2p::gossipsub::metrics::Config as GossipsubMetricsConfig;
use libp2p::gossipsub::subscription_filter::MaxCountSubscriptionFilter;
use libp2p::gossipsub::PublishError;
use libp2p::gossipsub::{
    Event as GossipsubEvent, IdentTopic as Topic, MessageAcceptance, MessageAuthenticity, MessageId,
};
use libp2p::multiaddr::{Multiaddr, Protocol as MProtocol};
use libp2p::swarm::{ConnectionLimits, Swarm, SwarmBuilder, SwarmEvent};
use libp2p::PeerId;
use libp2p::{relay, TransportExt};
use slog::{crit, debug, error, info, o, trace, warn};
use std::path::PathBuf;
use std::pin::Pin;
use std::{
    marker::PhantomData,
    sync::Arc,
    task::{Context, Poll},
};
use types::ForkName;
use types::{
    consts::altair::SYNC_COMMITTEE_SUBNET_COUNT, EnrForkId, EthSpec, ForkContext, Slot, SubnetId,
};
use utils::{build_nym_transport, Context as ServiceContext, MAX_CONNECTIONS_PER_PEER};

pub mod api_types;
mod behaviour;
mod gossip_cache;
pub mod gossipsub_scoring_parameters;
pub mod utils;
/// The number of peers we target per subnet for discovery queries.
pub const TARGET_SUBNET_PEERS: usize = 6;

const MAX_IDENTIFY_ADDRESSES: usize = 10;

/// The types of events than can be obtained from polling the behaviour.
#[derive(Debug)]
pub enum NetworkEvent<AppReqId: ReqId, TSpec: EthSpec> {
    /// We have successfully dialed and connected to a peer.
    PeerConnectedOutgoing(PeerId),
    /// A peer has successfully dialed and connected to us.
    PeerConnectedIncoming(PeerId),
    /// A peer has disconnected.
    PeerDisconnected(PeerId),
    /// The peer needs to be banned.
    PeerBanned(PeerId),
    /// The peer has been unbanned.
    PeerUnbanned(PeerId),
    /// An RPC Request that was sent failed.
    RPCFailed {
        /// The id of the failed request.
        id: AppReqId,
        /// The peer to which this request was sent.
        peer_id: PeerId,
    },
    RequestReceived {
        /// The peer that sent the request.
        peer_id: PeerId,
        /// Identifier of the request. All responses to this request must use this id.
        id: PeerRequestId,
        /// Request the peer sent.
        request: Request,
    },
    ResponseReceived {
        /// Peer that sent the response.
        peer_id: PeerId,
        /// Id of the request to which the peer is responding.
        id: AppReqId,
        /// Response the peer sent.
        response: Response<TSpec>,
    },
    PubsubMessage {
        /// The gossipsub message id. Used when propagating blocks after validation.
        id: MessageId,
        /// The peer from which we received this message, not the peer that published it.
        source: PeerId,
        /// The topic that this message was sent on.
        topic: TopicHash,
        /// The message itself.
        message: PubsubMessage<TSpec>,
    },
    /// Inform the network to send a Status to this peer.
    StatusPeer(PeerId),
    NewListenAddr(Multiaddr),
    ZeroListeners,
}

/// Builds the network behaviour that manages the core protocols of eth2.
/// This core behaviour is managed by `Behaviour` which adds peer management to all core
/// behaviours.
pub struct Network<AppReqId: ReqId, TSpec: EthSpec> {
    /// Nym address.
    pub address: Multiaddr,
    swarm: libp2p::swarm::Swarm<Behaviour<AppReqId, TSpec>>,
    /* Auxiliary Fields */
    /// A collections of variables accessible outside the network service.
    network_globals: Arc<NetworkGlobals<TSpec>>,
    /// Keeps track of the current EnrForkId for upgrading gossipsub topics.
    // NOTE: This can be accessed via the network_globals ENR. However we keep it here for quick
    // lookups for every gossipsub message send.
    enr_fork_id: EnrForkId,
    /// Directory where metadata is stored.
    network_dir: PathBuf,
    fork_context: Arc<ForkContext>,
    /// Gossipsub score parameters.
    score_settings: PeerScoreSettings<TSpec>,
    /// The interval for updating gossipsub scores
    update_gossipsub_scores: tokio::time::Interval,
    gossip_cache: GossipCache,
    /// The bandwidth logger for the underlying libp2p transport.
    pub bandwidth: Arc<BandwidthSinks>,
    /// This node's PeerId.
    pub local_peer_id: PeerId,
    /// Logger for behaviour actions.
    log: slog::Logger,
}

/// Implements the combined behaviour for the libp2p service.
impl<AppReqId: ReqId, TSpec: EthSpec> Network<AppReqId, TSpec> {
    pub async fn new(
        executor: task_executor::TaskExecutor,
        ctx: ServiceContext<'_>,
        log: &slog::Logger,
    ) -> error::Result<(Self, Arc<NetworkGlobals<TSpec>>)> {
        let log = log.new(o!("service"=> "libp2p"));
        let mut config = ctx.config.clone();
        trace!(log, "Libp2p Service starting");
        // initialise the node's ID
        let local_keypair = utils::load_private_key(&config, &log);

        // set up a collection of variables accessible outside of the network crate
        let network_globals = {
            // Create an ENR or load from disk if appropriate
            let enr = crate::discovery::enr::build_or_load_enr::<TSpec>(
                local_keypair.clone(),
                &config,
                &ctx.enr_fork_id,
                &log,
            )?;
            // Construct the metadata
            let meta_data = utils::load_or_build_metadata(&config.network_dir, &log);
            let globals = NetworkGlobals::new(
                enr,
                config.listen_addrs().v4().map(|v4_addr| v4_addr.tcp_port),
                config.listen_addrs().v6().map(|v6_addr| v6_addr.tcp_port),
                meta_data,
                config
                    .trusted_peers
                    .iter()
                    .map(|x| PeerId::from(x.clone()))
                    .collect(),
                &log,
            );
            Arc::new(globals)
        };

        // Grab our local ENR FORK ID
        let enr_fork_id = network_globals
            .local_enr()
            .eth2()
            .expect("Local ENR must have a fork id");

        let score_settings = PeerScoreSettings::new(ctx.chain_spec, &config.gs_config);

        let gossip_cache = {
            let slot_duration = std::time::Duration::from_secs(ctx.chain_spec.seconds_per_slot);
            let half_epoch = std::time::Duration::from_secs(
                ctx.chain_spec.seconds_per_slot * TSpec::slots_per_epoch() / 2,
            );

            GossipCache::builder()
                .beacon_block_timeout(slot_duration)
                .aggregates_timeout(half_epoch)
                .attestation_timeout(half_epoch)
                .voluntary_exit_timeout(half_epoch * 2)
                .proposer_slashing_timeout(half_epoch * 2)
                .attester_slashing_timeout(half_epoch * 2)
                // .signed_contribution_and_proof_timeout(timeout) // Do not retry
                // .sync_committee_message_timeout(timeout) // Do not retry
                .bls_to_execution_change_timeout(half_epoch * 2)
                .build()
        };

        let local_peer_id = network_globals.local_peer_id();

        let (gossipsub, update_gossipsub_scores) = {
            let thresholds = lighthouse_gossip_thresholds();

            // Prepare scoring parameters
            let params = {
                // Construct a set of gossipsub peer scoring parameters
                // We don't know the number of active validators and the current slot yet
                let active_validators = TSpec::minimum_validator_count();
                let current_slot = Slot::new(0);
                score_settings.get_peer_score_params(
                    active_validators,
                    &thresholds,
                    &enr_fork_id,
                    current_slot,
                )?
            };

            trace!(log, "Using peer score params"; "params" => ?params);

            // Set up a scoring update interval
            let update_gossipsub_scores = tokio::time::interval(params.decay_interval);

            let possible_fork_digests = ctx.fork_context.all_fork_digests();
            let filter = MaxCountSubscriptionFilter {
                filter: utils::create_whitelist_filter(
                    possible_fork_digests,
                    ctx.chain_spec.attestation_subnet_count,
                    SYNC_COMMITTEE_SUBNET_COUNT,
                ),
                max_subscribed_topics: 200,
                max_subscriptions_per_request: 150, // 148 in theory = (64 attestation + 4 sync committee + 6 core topics) * 2
            };

            config.gs_config = gossipsub_config(config.network_load, ctx.fork_context.clone());

            // If metrics are enabled for gossipsub build the configuration
            let gossipsub_metrics = ctx
                .gossipsub_registry
                .map(|registry| (registry, GossipsubMetricsConfig::default()))
                .unwrap();

            let snappy_transform = SnappyTransform::new(config.gs_config.max_transmit_size());

            let mut gossipsub = Gossipsub::new_with_subscription_filter_and_transform(
                MessageAuthenticity::Anonymous,
                config.gs_config.clone(),
                Some(gossipsub_metrics),
                filter,
                snappy_transform,
            )?;

            gossipsub
                .with_peer_score(params, thresholds)
                .expect("Valid score params and thresholds");

            (gossipsub, update_gossipsub_scores)
        };

        let eth2_rpc = RPC::new(
            ctx.fork_context.clone(),
            config.enable_light_client_server,
            config.outbound_rate_limiter_config.clone(),
            log.clone(),
        );

        let pm = PeerManager::new(network_globals.clone());

        let behaviour = {
            Behaviour {
                gossipsub,
                eth2_rpc,
                pm,
                relay: relay::Behaviour::new(local_peer_id, Default::default()),
            }
        };

        let (swarm, bandwidth, address) = {
            let nym_addr = config.nym_client_address.clone();
            let uri = format!("ws://{}:{}", nym_addr.addr, nym_addr.tcp_port);
            info!(log, "Connecting to nym client"; "address" => &uri);

            // TODO: Circuit relay
            // Set up the transport - tcp/ws with noise and mplex
            // let (transport, bandwidth) = match config.libp2p_transport {
            //     Libp2pTransport::Nym => build_nym_transport(local_keypair.clone(), uri).await,
            //     Libp2pTransport::Tcp => build_tcp_transport(local_keypair.clone()).await,
            //     Libp2pTransport::NymEitherTcp => build_transport(local_keypair.clone(), uri).await,
            // }
            let (transport, address) = build_nym_transport(local_keypair.clone(), uri)
                .await
                .map_err(|e| format!("Failed to build transport: {:?}", e))?;

            let (transport, bandwidth) = transport.with_bandwidth_logging();

            // use the executor for libp2p
            struct Executor(task_executor::TaskExecutor);
            impl libp2p::swarm::Executor for Executor {
                fn exec(&self, f: Pin<Box<dyn futures::Future<Output = ()> + Send>>) {
                    self.0.spawn(f, "libp2p");
                }
            }

            // sets up the libp2p connection limits
            let limits = ConnectionLimits::default()
                .with_max_pending_incoming(Some(5))
                .with_max_pending_outgoing(Some(16))
                .with_max_established_incoming(Some(
                    (config.target_peers as f32
                        * (1.0 + PEER_EXCESS_FACTOR - MIN_OUTBOUND_ONLY_FACTOR))
                        .ceil() as u32,
                ))
                .with_max_established_outgoing(Some(
                    (config.target_peers as f32 * (1.0 + PEER_EXCESS_FACTOR)).ceil() as u32,
                ))
                .with_max_established(Some(
                    (config.target_peers as f32 * (1.0 + PEER_EXCESS_FACTOR + PRIORITY_PEER_EXCESS))
                        .ceil() as u32,
                ))
                .with_max_established_per_peer(Some(MAX_CONNECTIONS_PER_PEER));

            (
                SwarmBuilder::with_executor(
                    transport,
                    behaviour,
                    local_peer_id,
                    Executor(executor),
                )
                .notify_handler_buffer_size(std::num::NonZeroUsize::new(7).expect("Not zero"))
                .per_connection_event_buffer_size(64)
                .connection_limits(limits)
                .build(),
                bandwidth,
                address,
            )
        };

        let mut network = Network {
            address,
            swarm,
            network_globals,
            enr_fork_id,
            network_dir: config.network_dir.clone(),
            fork_context: ctx.fork_context,
            score_settings,
            update_gossipsub_scores,
            gossip_cache,
            bandwidth,
            local_peer_id,
            log,
        };

        network.start(&config).await?;

        let network_globals = network.network_globals.clone();

        Ok((network, network_globals))
    }

    /// Starts the network:
    ///
    /// - Starts listening in the given ports.
    /// - Dials boot-nodes and libp2p peers.
    /// - Subscribes to starting gossipsub topics.
    async fn start(&mut self, config: &crate::NetworkConfig) -> error::Result<()> {
        let enr = self.network_globals.local_enr();
        info!(self.log, "Libp2p Starting"; "peer_id" => %enr.peer_id(), "bandwidth_config" => format!("{}-{}", config.network_load, NetworkLoad::from(config.network_load).name));
        debug!(self.log, "Attempting to open listening ports"; config.listen_addrs(), "discovery_enabled" => !config.disable_discovery);

        // listen on nym address
        let listen_multiaddr = self.address.clone();
        match self.swarm.listen_on(listen_multiaddr.clone()) {
            Ok(_) => {
                let mut log_address = listen_multiaddr.clone();
                log_address.push(MProtocol::P2p(enr.peer_id().into()));
                info!(self.log, "Listening established"; "address" => %listen_multiaddr.clone());
            }
            Err(err) => {
                crit!(
                    self.log,
                    "Unable to listen on libp2p address";
                    "error" => ?err,
                    "listen_multiaddr" => %listen_multiaddr,
                );
                return Err("Libp2p was unable to listen on the given listen address.".into());
            }
        };

        let mut subscribed_topics: Vec<GossipKind> = vec![];

        for topic_kind in &config.topics {
            if self.subscribe_kind(topic_kind.clone()) {
                subscribed_topics.push(topic_kind.clone());
            } else {
                warn!(self.log, "Could not subscribe to topic"; "topic" => %topic_kind);
            }
        }

        if !subscribed_topics.is_empty() {
            info!(self.log, "Subscribed to topics"; "topics" => ?subscribed_topics);
        }

        Ok(())
    }

    /* Public Accessible Functions to interact with the behaviour */

    /// The routing pub-sub mechanism for eth2.
    pub fn gossipsub_mut(&mut self) -> &mut Gossipsub {
        &mut self.swarm.behaviour_mut().gossipsub
    }
    /// The Eth2 RPC specified in the wire-0 protocol.
    pub fn eth2_rpc_mut(&mut self) -> &mut RPC<RequestId<AppReqId>, TSpec> {
        &mut self.swarm.behaviour_mut().eth2_rpc
    }

    /// The routing pub-sub mechanism for eth2.
    pub fn gossipsub(&self) -> &Gossipsub {
        &self.swarm.behaviour().gossipsub
    }
    /// The Eth2 RPC specified in the wire-0 protocol.
    pub fn eth2_rpc(&self) -> &RPC<RequestId<AppReqId>, TSpec> {
        &self.swarm.behaviour().eth2_rpc
    }

    /// Returns the local ENR of the node.
    pub fn local_enr(&self) -> Enr {
        self.network_globals.local_enr()
    }

    /* Pubsub behaviour functions */

    /// Subscribes to a gossipsub topic kind, letting the network service determine the
    /// encoding and fork version.
    pub fn subscribe_kind(&mut self, kind: GossipKind) -> bool {
        let gossip_topic = GossipTopic::new(
            kind,
            GossipEncoding::default(),
            self.enr_fork_id.fork_digest,
        );

        self.subscribe(gossip_topic)
    }

    /// Unsubscribes from a gossipsub topic kind, letting the network service determine the
    /// encoding and fork version.
    pub fn unsubscribe_kind(&mut self, kind: GossipKind) -> bool {
        let gossip_topic = GossipTopic::new(
            kind,
            GossipEncoding::default(),
            self.enr_fork_id.fork_digest,
        );
        self.unsubscribe(gossip_topic)
    }

    /// Subscribe to all required topics for the `new_fork` with the given `new_fork_digest`.
    pub fn subscribe_new_fork_topics(&mut self, new_fork: ForkName, new_fork_digest: [u8; 4]) {
        // Subscribe to existing topics with new fork digest
        let subscriptions = self.network_globals.gossipsub_subscriptions.read().clone();
        for mut topic in subscriptions.into_iter() {
            topic.fork_digest = new_fork_digest;
            self.subscribe(topic);
        }

        // Subscribe to core topics for the new fork
        for kind in fork_core_topics(&new_fork) {
            let topic = GossipTopic::new(kind, GossipEncoding::default(), new_fork_digest);
            self.subscribe(topic);
        }
    }

    /// Unsubscribe from all topics that doesn't have the given fork_digest
    pub fn unsubscribe_from_fork_topics_except(&mut self, except: [u8; 4]) {
        let subscriptions = self.network_globals.gossipsub_subscriptions.read().clone();
        for topic in subscriptions
            .iter()
            .filter(|topic| topic.fork_digest != except)
            .cloned()
        {
            self.unsubscribe(topic);
        }
    }

    /// Subscribes to a gossipsub topic.
    ///
    /// Returns `true` if the subscription was successful and `false` otherwise.
    pub fn subscribe(&mut self, topic: GossipTopic) -> bool {
        // update the network globals
        self.network_globals
            .gossipsub_subscriptions
            .write()
            .insert(topic.clone());

        let topic: Topic = topic.into();

        match self.gossipsub_mut().subscribe(&topic) {
            Err(e) => {
                warn!(self.log, "Failed to subscribe to topic"; "topic" => %topic, "error" => ?e);
                false
            }
            Ok(_) => {
                debug!(self.log, "Subscribed to topic"; "topic" => %topic);
                true
            }
        }
    }

    /// Unsubscribe from a gossipsub topic.
    pub fn unsubscribe(&mut self, topic: GossipTopic) -> bool {
        // update the network globals
        self.network_globals
            .gossipsub_subscriptions
            .write()
            .remove(&topic);

        // unsubscribe from the topic
        let libp2p_topic: Topic = topic.clone().into();

        match self.gossipsub_mut().unsubscribe(&libp2p_topic) {
            Err(_) => {
                warn!(self.log, "Failed to unsubscribe from topic"; "topic" => %libp2p_topic);
                false
            }
            Ok(v) => {
                // Inform the network
                debug!(self.log, "Unsubscribed to topic"; "topic" => %topic);
                v
            }
        }
    }

    /// Publishes a list of messages on the pubsub (gossipsub) behaviour, choosing the encoding.
    pub fn publish(&mut self, messages: Vec<PubsubMessage<TSpec>>) {
        for message in messages {
            for topic in message.topics(GossipEncoding::default(), self.enr_fork_id.fork_digest) {
                let message_data = message.encode(GossipEncoding::default());
                if let Err(e) = self
                    .gossipsub_mut()
                    .publish(Topic::from(topic.clone()), message_data.clone())
                {
                    slog::warn!(self.log, "Could not publish message"; "error" => ?e);

                    // add to metrics
                    match topic.kind() {
                        GossipKind::Attestation(subnet_id) => {
                            if let Some(v) = metrics::get_int_gauge(
                                &metrics::FAILED_ATTESTATION_PUBLISHES_PER_SUBNET,
                                &[subnet_id.as_ref()],
                            ) {
                                v.inc()
                            };
                        }
                        kind => {
                            if let Some(v) = metrics::get_int_gauge(
                                &metrics::FAILED_PUBLISHES_PER_MAIN_TOPIC,
                                &[&format!("{:?}", kind)],
                            ) {
                                v.inc()
                            };
                        }
                    }

                    if let PublishError::InsufficientPeers = e {
                        self.gossip_cache.insert(topic, message_data);
                    }
                }
            }
        }
    }

    /// Informs the gossipsub about the result of a message validation.
    /// If the message is valid it will get propagated by gossipsub.
    pub fn report_message_validation_result(
        &mut self,
        propagation_source: &PeerId,
        message_id: MessageId,
        validation_result: MessageAcceptance,
    ) {
        if let Some(result) = match validation_result {
            MessageAcceptance::Accept => None,
            MessageAcceptance::Ignore => Some("ignore"),
            MessageAcceptance::Reject => Some("reject"),
        } {
            if let Some(client) = self
                .network_globals
                .peers
                .read()
                .peer_info(propagation_source)
                .map(|info| info.client().kind.as_ref())
            {
                metrics::inc_counter_vec(
                    &metrics::GOSSIP_UNACCEPTED_MESSAGES_PER_CLIENT,
                    &[client, result],
                )
            }
        }

        if let Err(e) = self.gossipsub_mut().report_message_validation_result(
            &message_id,
            propagation_source,
            validation_result,
        ) {
            warn!(self.log, "Failed to report message validation"; "message_id" => %message_id, "peer_id" => %propagation_source, "error" => ?e);
        }
    }

    /// Updates the current gossipsub scoring parameters based on the validator count and current
    /// slot.
    pub fn update_gossipsub_parameters(
        &mut self,
        active_validators: usize,
        current_slot: Slot,
    ) -> error::Result<()> {
        let (beacon_block_params, beacon_aggregate_proof_params, beacon_attestation_subnet_params) =
            self.score_settings
                .get_dynamic_topic_params(active_validators, current_slot)?;

        let fork_digest = self.enr_fork_id.fork_digest;
        let get_topic = |kind: GossipKind| -> Topic {
            GossipTopic::new(kind, GossipEncoding::default(), fork_digest).into()
        };

        debug!(self.log, "Updating gossipsub score parameters";
            "active_validators" => active_validators);
        trace!(self.log, "Updated gossipsub score parameters";
            "beacon_block_params" => ?beacon_block_params,
            "beacon_aggregate_proof_params" => ?beacon_aggregate_proof_params,
            "beacon_attestation_subnet_params" => ?beacon_attestation_subnet_params,
        );

        self.gossipsub_mut()
            .set_topic_params(get_topic(GossipKind::BeaconBlock), beacon_block_params)?;

        self.gossipsub_mut().set_topic_params(
            get_topic(GossipKind::BeaconAggregateAndProof),
            beacon_aggregate_proof_params,
        )?;

        for i in 0..self.score_settings.attestation_subnet_count() {
            self.gossipsub_mut().set_topic_params(
                get_topic(GossipKind::Attestation(SubnetId::new(i))),
                beacon_attestation_subnet_params.clone(),
            )?;
        }

        Ok(())
    }

    /* Eth2 RPC behaviour functions */

    /// Send a request to a peer over RPC.
    pub fn send_request(&mut self, peer_id: PeerId, request_id: AppReqId, request: Request) {
        self.eth2_rpc_mut().send_request(
            peer_id,
            RequestId::Application(request_id),
            request.into(),
        )
    }

    /// Send a successful response to a peer over RPC.
    pub fn send_response(&mut self, peer_id: PeerId, id: PeerRequestId, response: Response<TSpec>) {
        self.eth2_rpc_mut()
            .send_response(peer_id, id, response.into())
    }

    /// Inform the peer that their request produced an error.
    pub fn send_error_reponse(
        &mut self,
        peer_id: PeerId,
        id: PeerRequestId,
        error: RPCResponseErrorCode,
        reason: String,
    ) {
        self.eth2_rpc_mut().send_response(
            peer_id,
            id,
            RPCCodedResponse::Error(error, reason.into()),
        )
    }

    /* Peer management functions */

    pub fn testing_dial(&mut self, addr: Multiaddr) -> Result<(), libp2p::swarm::DialError> {
        self.swarm.dial(addr)
    }

    /// Sends a Ping request to the peer.
    fn ping(&mut self, peer_id: PeerId) {
        let ping = crate::rpc::Ping {
            data: *self.network_globals.local_metadata.read().seq_number(),
        };
        trace!(self.log, "Sending Ping"; "peer_id" => %peer_id);
        let id = RequestId::Internal;
        self.eth2_rpc_mut()
            .send_request(peer_id, id, OutboundRequest::Ping(ping));
    }

    /// Sends a Pong response to the peer.
    fn pong(&mut self, id: PeerRequestId, peer_id: PeerId) {
        let ping = crate::rpc::Ping {
            data: *self.network_globals.local_metadata.read().seq_number(),
        };
        trace!(self.log, "Sending Pong"; "request_id" => id.1, "peer_id" => %peer_id);
        let event = RPCCodedResponse::Success(RPCResponse::Pong(ping));
        self.eth2_rpc_mut().send_response(peer_id, id, event);
    }

    /// Sends a METADATA request to a peer.
    fn send_meta_data_request(&mut self, peer_id: PeerId) {
        let event = OutboundRequest::MetaData(PhantomData);
        self.eth2_rpc_mut()
            .send_request(peer_id, RequestId::Internal, event);
    }

    /// Sends a METADATA response to a peer.
    fn send_meta_data_response(&mut self, id: PeerRequestId, peer_id: PeerId) {
        let event = RPCCodedResponse::Success(RPCResponse::MetaData(
            self.network_globals.local_metadata.read().clone(),
        ));
        self.eth2_rpc_mut().send_response(peer_id, id, event);
    }

    // RPC Propagation methods
    /// Queues the response to be sent upwards as long at it was requested outside the Behaviour.
    #[must_use = "return the response"]
    fn build_response(
        &mut self,
        id: RequestId<AppReqId>,
        peer_id: PeerId,
        response: Response<TSpec>,
    ) -> Option<NetworkEvent<AppReqId, TSpec>> {
        match id {
            RequestId::Application(id) => Some(NetworkEvent::ResponseReceived {
                peer_id,
                id,
                response,
            }),
            RequestId::Internal => None,
        }
    }

    /// Convenience function to propagate a request.
    #[must_use = "actually return the event"]
    fn build_request(
        &mut self,
        id: PeerRequestId,
        peer_id: PeerId,
        request: Request,
    ) -> NetworkEvent<AppReqId, TSpec> {
        // Increment metrics
        match &request {
            Request::Status(_) => {
                metrics::inc_counter_vec(&metrics::TOTAL_RPC_REQUESTS, &["status"])
            }
            Request::LightClientBootstrap(_) => {
                metrics::inc_counter_vec(&metrics::TOTAL_RPC_REQUESTS, &["light_client_bootstrap"])
            }
            Request::BlocksByRange { .. } => {
                metrics::inc_counter_vec(&metrics::TOTAL_RPC_REQUESTS, &["blocks_by_range"])
            }
            Request::BlocksByRoot { .. } => {
                metrics::inc_counter_vec(&metrics::TOTAL_RPC_REQUESTS, &["blocks_by_root"])
            }
        }
        NetworkEvent::RequestReceived {
            peer_id,
            id,
            request,
        }
    }

    /* Sub-behaviour event handling functions */

    /// Handle a gossipsub event.
    fn inject_gs_event(&mut self, event: GossipsubEvent) -> Option<NetworkEvent<AppReqId, TSpec>> {
        match event {
            GossipsubEvent::Message {
                propagation_source,
                message_id: id,
                message: gs_msg,
            } => {
                // Note: We are keeping track here of the peer that sent us the message, not the
                // peer that originally published the message.
                match PubsubMessage::decode(&gs_msg.topic, &gs_msg.data, &self.fork_context) {
                    Err(e) => {
                        debug!(self.log, "Could not decode gossipsub message"; "topic" => ?gs_msg.topic,"error" => e);
                        //reject the message
                        if let Err(e) = self.gossipsub_mut().report_message_validation_result(
                            &id,
                            &propagation_source,
                            MessageAcceptance::Reject,
                        ) {
                            warn!(self.log, "Failed to report message validation"; "message_id" => %id, "peer_id" => %propagation_source, "error" => ?e);
                        }
                    }
                    Ok(msg) => {
                        // Notify the network
                        return Some(NetworkEvent::PubsubMessage {
                            id,
                            source: propagation_source,
                            topic: gs_msg.topic,
                            message: msg,
                        });
                    }
                }
            }
            GossipsubEvent::Subscribed { peer_id, topic } => {
                if let Ok(topic) = GossipTopic::decode(topic.as_str()) {
                    if let Some(subnet_id) = topic.subnet_id() {
                        self.network_globals
                            .peers
                            .write()
                            .add_subscription(&peer_id, subnet_id);
                    }
                    // Try to send the cached messages for this topic
                    if let Some(msgs) = self.gossip_cache.retrieve(&topic) {
                        for data in msgs {
                            let topic_str: &str = topic.kind().as_ref();
                            match self
                                .swarm
                                .behaviour_mut()
                                .gossipsub
                                .publish(Topic::from(topic.clone()), data)
                            {
                                Ok(_) => {
                                    warn!(self.log, "Gossip message published on retry"; "topic" => topic_str);
                                    if let Some(v) = metrics::get_int_counter(
                                        &metrics::GOSSIP_LATE_PUBLISH_PER_TOPIC_KIND,
                                        &[topic_str],
                                    ) {
                                        v.inc()
                                    };
                                }
                                Err(e) => {
                                    warn!(self.log, "Gossip message publish failed on retry"; "topic" => topic_str, "error" => %e);
                                    if let Some(v) = metrics::get_int_counter(
                                        &metrics::GOSSIP_FAILED_LATE_PUBLISH_PER_TOPIC_KIND,
                                        &[topic_str],
                                    ) {
                                        v.inc()
                                    };
                                }
                            }
                        }
                    }
                }
            }
            GossipsubEvent::Unsubscribed { peer_id, topic } => {
                if let Some(subnet_id) = subnet_from_topic_hash(&topic) {
                    self.network_globals
                        .peers
                        .write()
                        .remove_subscription(&peer_id, &subnet_id);
                }
            }
            GossipsubEvent::GossipsubNotSupported { peer_id } => {
                debug!(self.log, "Peer does not support gossipsub"; "peer_id" => %peer_id);
                // self.peer_manager_mut().report_peer(
                //     &peer_id,
                //     PeerAction::LowToleranceError,
                //     ReportSource::Gossipsub,
                //     Some(GoodbyeReason::Unknown),
                //     "does_not_support_gossipsub",
                // );
            }
        }
        None
    }

    /// Handle an RPC event.
    fn inject_rpc_event(
        &mut self,
        event: RPCMessage<RequestId<AppReqId>, TSpec>,
    ) -> Option<NetworkEvent<AppReqId, TSpec>> {
        let peer_id = event.peer_id;

        if !self.swarm.is_connected(&peer_id) {
            debug!(
                self.log,
                "Ignoring rpc message of disconnecting peer";
                event
            );
            return None;
        }

        let handler_id = event.conn_id;
        // The METADATA and PING RPC responses are handled within the behaviour and not propagated
        match event.event {
            Err(handler_err) => {
                match handler_err {
                    HandlerErr::Inbound {
                        id: _,
                        proto: _,
                        error: _,
                    } => {
                        // // Inform the peer manager of the error.
                        // // An inbound error here means we sent an error to the peer, or the stream
                        // // timed out.
                        // self.peer_manager_mut().handle_rpc_error(
                        //     &peer_id,
                        //     proto,
                        //     &error,
                        //     ConnectionDirection::Incoming,
                        // );
                        None
                    }
                    HandlerErr::Outbound {
                        id,
                        proto: _,
                        error: _,
                    } => {
                        // // Inform the peer manager that a request we sent to the peer failed
                        // self.peer_manager_mut().handle_rpc_error(
                        //     &peer_id,
                        //     proto,
                        //     &error,
                        //     ConnectionDirection::Outgoing,
                        // );
                        // inform failures of requests comming outside the behaviour
                        if let RequestId::Application(id) = id {
                            Some(NetworkEvent::RPCFailed { peer_id, id })
                        } else {
                            None
                        }
                    }
                }
            }
            Ok(RPCReceived::Request(id, request)) => {
                let peer_request_id = (handler_id, id);
                match request {
                    /* Behaviour managed protocols: Ping and Metadata */
                    InboundRequest::Ping(_) => {
                        // // inform the peer manager and send the response
                        // self.peer_manager_mut().ping_request(&peer_id, ping.data);
                        // send a ping response
                        self.pong(peer_request_id, peer_id);
                        None
                    }
                    InboundRequest::MetaData(_) => {
                        // send the requested meta-data
                        self.send_meta_data_response((handler_id, id), peer_id);
                        None
                    }
                    InboundRequest::Goodbye(reason) => {
                        // queue for disconnection without a goodbye message
                        debug!(
                            self.log, "Peer sent Goodbye";
                            "peer_id" => %peer_id,
                            "reason" => %reason,
                            "client" => %self.network_globals.client(&peer_id),
                        );
                        // NOTE: We currently do not inform the application that we are
                        // disconnecting here. The RPC handler will automatically
                        // disconnect for us.
                        // The actual disconnection event will be relayed to the application.
                        None
                    }
                    /* Protocols propagated to the Network */
                    InboundRequest::Status(msg) => {
                        // // inform the peer manager that we have received a status from a peer
                        // self.peer_manager_mut().peer_statusd(&peer_id);
                        // propagate the STATUS message upwards
                        let event =
                            self.build_request(peer_request_id, peer_id, Request::Status(msg));
                        Some(event)
                    }
                    InboundRequest::BlocksByRange(req) => {
                        let methods::OldBlocksByRangeRequest {
                            start_slot,
                            mut count,
                            step,
                        } = req;
                        // Still disconnect the peer if the request is naughty.
                        if step == 0 {
                            // self.peer_manager_mut().handle_rpc_error(
                            //     &peer_id,
                            //     Protocol::BlocksByRange,
                            //     &RPCError::InvalidData(
                            //         "Blocks by range with 0 step parameter".into(),
                            //     ),
                            //     ConnectionDirection::Incoming,
                            // );
                            return None;
                        }
                        // return just one block in case the step parameter is used. https://github.com/ethereum/consensus-specs/pull/2856
                        if step > 1 {
                            count = 1;
                        }
                        let event = self.build_request(
                            peer_request_id,
                            peer_id,
                            Request::BlocksByRange(BlocksByRangeRequest { start_slot, count }),
                        );
                        Some(event)
                    }
                    InboundRequest::BlocksByRoot(req) => {
                        let event = self.build_request(
                            peer_request_id,
                            peer_id,
                            Request::BlocksByRoot(req),
                        );
                        Some(event)
                    }
                    InboundRequest::LightClientBootstrap(req) => {
                        let event = self.build_request(
                            peer_request_id,
                            peer_id,
                            Request::LightClientBootstrap(req),
                        );
                        Some(event)
                    }
                }
            }
            Ok(RPCReceived::Response(id, resp)) => {
                match resp {
                    /* Behaviour managed protocols */
                    RPCResponse::Pong(_) => {
                        // self.peer_manager_mut().pong_response(&peer_id, ping.data);
                        None
                    }
                    RPCResponse::MetaData(_) => {
                        // self.peer_manager_mut()
                        //     .meta_data_response(&peer_id, meta_data);
                        None
                    }
                    /* Network propagated protocols */
                    RPCResponse::Status(msg) => {
                        // // inform the peer manager that we have received a status from a peer
                        // self.peer_manager_mut().peer_statusd(&peer_id);
                        // propagate the STATUS message upwards
                        self.build_response(id, peer_id, Response::Status(msg))
                    }
                    RPCResponse::BlocksByRange(resp) => {
                        self.build_response(id, peer_id, Response::BlocksByRange(Some(resp)))
                    }
                    RPCResponse::BlocksByRoot(resp) => {
                        self.build_response(id, peer_id, Response::BlocksByRoot(Some(resp)))
                    }
                    // Should never be reached
                    RPCResponse::LightClientBootstrap(bootstrap) => {
                        self.build_response(id, peer_id, Response::LightClientBootstrap(bootstrap))
                    }
                }
            }
            Ok(RPCReceived::EndOfStream(id, termination)) => {
                let response = match termination {
                    ResponseTermination::BlocksByRange => Response::BlocksByRange(None),
                    ResponseTermination::BlocksByRoot => Response::BlocksByRoot(None),
                };
                self.build_response(id, peer_id, response)
            }
        }
    }

    /// Handle an pm event.
    fn inject_pm_event(
        &mut self,
        peers: Vec<(PeerId, Multiaddr)>,
    ) -> Option<NetworkEvent<AppReqId, TSpec>> {
        for (peer_id, multiaddr) in peers {
            if self.swarm.is_connected(&peer_id) || self.swarm.is_dialing(&peer_id) {
                continue;
            }

            self.swarm.dial(multiaddr).unwrap_or_else(
                |e| error!(self.log, "Could not dial address"; "error" => format!("{:?}", e)),
            );
        }

        None
    }

    /* Networking polling */

    /// Poll the p2p networking stack.
    ///
    /// This will poll the swarm and do maintenance routines.
    pub fn poll_network(&mut self, cx: &mut Context) -> Poll<NetworkEvent<AppReqId, TSpec>> {
        while let Poll::Ready(Some(swarm_event)) = self.swarm.poll_next_unpin(cx) {
            let maybe_event = match swarm_event {
                SwarmEvent::Behaviour(behaviour_event) => match behaviour_event {
                    // Handle sub-behaviour events.
                    BehaviourEvent::Gossipsub(ge) => self.inject_gs_event(ge),
                    BehaviourEvent::Eth2Rpc(re) => self.inject_rpc_event(re),
                    BehaviourEvent::Pm(pm) => self.inject_pm_event(pm),
                    BehaviourEvent::Relay(_) => todo!(),
                },
                SwarmEvent::ConnectionEstablished { .. } => None,
                SwarmEvent::ConnectionClosed { .. } => None,
                SwarmEvent::IncomingConnection {
                    local_addr,
                    send_back_addr,
                } => {
                    warn!(self.log, "Incoming connection"; "our_addr" => %local_addr, "from" => %send_back_addr);
                    None
                }
                SwarmEvent::IncomingConnectionError {
                    local_addr,
                    send_back_addr,
                    error,
                } => {
                    error!(self.log, "Failed incoming connection"; "our_addr" => %local_addr, "from" => %send_back_addr, "error" => %error);
                    None
                }
                SwarmEvent::OutgoingConnectionError { peer_id, error } => {
                    error!(self.log, "Failed to dial address"; "peer_id" => ?peer_id,  "error" => %error);
                    None
                }
                SwarmEvent::BannedPeer {
                    peer_id,
                    endpoint: _,
                } => {
                    warn!(self.log, "Banned peer connection rejected"; "peer_id" => %peer_id);
                    None
                }
                SwarmEvent::NewListenAddr { address, .. } => {
                    Some(NetworkEvent::NewListenAddr(address))
                }
                SwarmEvent::ExpiredListenAddr { address, .. } => {
                    warn!(self.log, "Listen address expired"; "address" => %address);
                    None
                }
                SwarmEvent::ListenerClosed {
                    addresses, reason, ..
                } => {
                    crit!(self.log, "Listener closed"; "addresses" => ?addresses, "reason" => ?reason);
                    if Swarm::listeners(&self.swarm).count() == 0 {
                        Some(NetworkEvent::ZeroListeners)
                    } else {
                        None
                    }
                }
                SwarmEvent::ListenerError { error, .. } => {
                    // this is non fatal, but we still check
                    error!(self.log, "Listener error"; "error" => ?error);
                    if Swarm::listeners(&self.swarm).count() == 0 {
                        Some(NetworkEvent::ZeroListeners)
                    } else {
                        None
                    }
                }
                SwarmEvent::Dialing(_) => None,
            };

            if let Some(ev) = maybe_event {
                return Poll::Ready(ev);
            }
        }

        // perform gossipsub score updates when necessary
        while self.update_gossipsub_scores.poll_tick(cx).is_ready() {
            // let this = self.swarm.behaviour_mut();
            // this.peer_manager.update_gossipsub_scores(&this.gossipsub);
        }

        // poll the gossipsub cache to clear expired messages
        while let Poll::Ready(Some(result)) = self.gossip_cache.poll_next_unpin(cx) {
            match result {
                Err(e) => warn!(self.log, "Gossip cache error"; "error" => e),
                Ok(expired_topic) => {
                    if let Some(v) = metrics::get_int_counter(
                        &metrics::GOSSIP_EXPIRED_LATE_PUBLISH_PER_TOPIC_KIND,
                        &[expired_topic.kind().as_ref()],
                    ) {
                        v.inc()
                    };
                }
            }
        }
        Poll::Pending
    }

    pub async fn next_event(&mut self) -> NetworkEvent<AppReqId, TSpec> {
        futures::future::poll_fn(|cx| self.poll_network(cx)).await
    }
}
