use crate::pm::PeerManager;
use crate::rpc::{ReqId, RPC};
use crate::types::SnappyTransform;

use libp2p::gossipsub::subscription_filter::{
    MaxCountSubscriptionFilter, WhitelistSubscriptionFilter,
};
use libp2p::gossipsub::Behaviour as BaseGossipsub;
use libp2p::relay;
use libp2p::swarm::NetworkBehaviour;
use types::EthSpec;

use super::api_types::RequestId;

pub type SubscriptionFilter = MaxCountSubscriptionFilter<WhitelistSubscriptionFilter>;
pub type Gossipsub = BaseGossipsub<SnappyTransform, SubscriptionFilter>;

#[derive(NetworkBehaviour)]
pub(crate) struct Behaviour<AppReqId, TSpec>
where
    AppReqId: ReqId,
    TSpec: EthSpec,
{
    /// The routing pub-sub mechanism for eth2.
    pub gossipsub: Gossipsub,
    /// The Eth2 RPC specified in the wire-0 protocol.
    pub eth2_rpc: RPC<RequestId<AppReqId>, TSpec>,
    /// Keep regular connection to peers and disconnect if absent.
    // NOTE: The id protocol is used for initial interop. This will be removed by mainnet.
    /// Circuit relay for nodes supporting Nym and TCP.
    pub relay: relay::Behaviour,

    pub pm: PeerManager<TSpec>,
}
