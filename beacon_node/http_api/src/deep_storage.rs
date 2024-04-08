use serde::{Deserialize, Serialize};
use ssz::Decode;
use types::BeaconBlockHeader;
use types::ChainSpec;
use types::ForkName;
use types::ForkVersionDeserialize;
use types::{light_client_update::*, BeaconBlockBody};
use types::{
    test_utils::TestRandom, EthSpec,
    FixedVector, Hash256, SignedBeaconBlock,
};
// use ssz_derive::{Decode, Encode};
use std::marker::PhantomData;
use superstruct::superstruct;
use types::ExecPayload;

#[superstruct(
    variants(Base, Altair, Merge, Capella, Deneb),
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
#[serde(bound = "E: EthSpec", deny_unknown_fields)]
// #[arbitrary(bound = "E: EthSpec")]
pub struct DeepStorageBlock<E: EthSpec> {
    pub block_root: Hash256,
    pub block_root_branch: Vec<Hash256>,

    #[superstruct(only(Merge), partial_getter(rename = "execution_payload_header_merge"))]
    pub execution: types::ExecutionPayloadHeaderMerge<E>,
    #[superstruct(
        only(Capella),
        partial_getter(rename = "execution_payload_header_capella")
    )]
    pub execution: types::ExecutionPayloadHeaderCapella<E>,
    #[superstruct(only(Deneb), partial_getter(rename = "execution_payload_header_deneb"))]
    pub execution: types::ExecutionPayloadHeaderDeneb<E>,

    #[superstruct(only(Merge, Capella, Deneb))]
    pub execution_branch: Vec<Hash256>,
    #[serde(skip)]
    // #[arbitrary(default)]
    pub _phantom_data: PhantomData<E>,
}

impl<E: EthSpec> DeepStorageBlockMerge<E> {
    pub fn new(
        block: &SignedBeaconBlock<E>,
        block_root_branch: Vec<Hash256>,
    ) -> Result<Self, warp::Rejection> {
        let beacon_block_body = BeaconBlockBody::from(
            block
                .message()
                .body_merge()
                .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))?
                .to_owned(),
        );

        let execution = block
            .message()
            .execution_payload()
            .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))?
            .to_execution_payload_header();

        let execution_branch = beacon_block_body
            .block_body_merkle_proof(EXECUTION_PAYLOAD_INDEX)
            .unwrap();

        Ok(Self {
            block_root: block.canonical_root(),
            execution: execution.as_merge().unwrap().clone(),
            execution_branch,
            block_root_branch,
            _phantom_data: PhantomData,
        })
    }
}

impl<E: EthSpec> DeepStorageBlockCapella<E> {
    pub fn new(
        block: &SignedBeaconBlock<E>,
        block_root_branch: Vec<Hash256>,
    ) -> Result<Self, warp::Rejection> {
        let beacon_block_body = BeaconBlockBody::from(
            block
                .message()
                .body_capella()
                .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))?
                .to_owned(),
        );

        let execution = block
            .message()
            .execution_payload()
            .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))?
            .to_execution_payload_header();

        let execution_branch = beacon_block_body
            .block_body_merkle_proof(EXECUTION_PAYLOAD_INDEX)
            .unwrap();

        Ok(Self {
            block_root: block.canonical_root(),
            execution: execution.as_capella().unwrap().clone(),
            execution_branch,
            block_root_branch,
            _phantom_data: PhantomData,
        })
    }
}

impl<E: EthSpec> DeepStorageBlockDeneb<E> {
    pub fn new(
        block: &SignedBeaconBlock<E>,
        block_root_branch: Vec<Hash256>,
    ) -> Result<Self, warp::Rejection> {
        let beacon_block_body = BeaconBlockBody::from(
            block
                .message()
                .body_deneb()
                .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))?
                .to_owned(),
        );

        let execution = block
            .message()
            .execution_payload()
            .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))?
            .to_execution_payload_header();

        let execution_branch = beacon_block_body
            .block_body_merkle_proof(EXECUTION_PAYLOAD_INDEX)
            .unwrap();

        Ok(Self {
            block_root: block.canonical_root(),
            execution: execution.as_deneb().unwrap().clone(),
            execution_branch,
            block_root_branch,
            _phantom_data: PhantomData,
        })
    }
}
