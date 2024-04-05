use types::BeaconBlockHeader;
use types::ChainSpec;
use types::ForkName;
use types::ForkVersionDeserialize;
use types::{light_client_update::*, BeaconBlockBody};
use types::{
    test_utils::TestRandom, EthSpec, ExecutionPayloadHeaderCapella, ExecutionPayloadHeaderDeneb,
    FixedVector, Hash256, SignedBeaconBlock,
};
use serde::{Deserialize, Serialize};
use ssz::Decode;
// use ssz_derive::{Decode, Encode};
use std::marker::PhantomData;
use superstruct::superstruct;

#[superstruct(
    variants(Altair, Capella, Deneb),
    variant_attributes(
        derive(
            Debug,
            Clone,
            PartialEq,
            Serialize,
            Deserialize,
            // Decode,
            // Encode,
            // TestRandom,
            // arbitrary::Arbitrary,
        ),
        serde(bound = "E: EthSpec", deny_unknown_fields),
        // arbitrary(bound = "E: EthSpec"),
    )
)]
#[derive(
    Debug, Clone, Serialize,Deserialize, PartialEq,
)]
#[serde(untagged)]
#[serde(bound = "E: EthSpec", deny_unknown_fields)]
// #[arbitrary(bound = "E: EthSpec")]
pub struct DeepStorageBlock<E: EthSpec> {
    pub block_root: Hash256,
    pub block_root_branch: Vec<Hash256>,

    #[superstruct(
        only(Capella),
        partial_getter(rename = "execution_payload_header_capella")
    )]
    pub execution: ExecutionPayloadHeaderCapella<E>,
    #[superstruct(only(Deneb), partial_getter(rename = "execution_payload_header_deneb"))]
    pub execution: ExecutionPayloadHeaderDeneb<E>,

    #[superstruct(only(Capella, Deneb))]
    pub execution_branch: Vec<Hash256>,

    #[serde(skip)]
    // #[arbitrary(default)]
    pub _phantom_data: PhantomData<E>,
}
