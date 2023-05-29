use crate::discv::Discv;
use crate::peer_manager::PeerManager;
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

    /// Circuit relay for nodes supporting Nym and TCP.
    pub relay: relay::Behaviour,

    pub discv: Discv<TSpec>,

    /// The peer manager that keeps track of peer's reputation and status.
    pub peer_manager: PeerManager<TSpec>,
}
