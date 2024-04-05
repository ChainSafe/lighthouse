use crate::deep_storage::DeepStorageBlockCapella;
use crate::version::inconsistent_fork_rejection;
use crate::{state_id::checkpoint_slot_and_execution_optimistic, ExecutionOptimistic};
use beacon_chain::{BeaconChain, BeaconChainError, BeaconChainTypes, WhenSlotSkipped};
use eth2::types::BlobIndicesQuery;
use eth2::types::BlockId as CoreBlockId;
use lighthouse_network::discv5::enr::k256::elliptic_curve::rand_core::block;
use merkle_proof::MerkleTree;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use types::ExecPayload;
use crate::deep_storage::DeepStorageBlock;
use types::light_client_update::EXECUTION_PAYLOAD_INDEX;
use types::{BeaconBlockBody, ForkName};
use types::{BlobSidecarList, EthSpec, Hash256, SignedBeaconBlock, SignedBlindedBeaconBlock, Slot};


/// Wraps `eth2::types::BlockId` and provides a simple way to obtain a block or root for a given
/// `BlockId`.
#[derive(Debug)]
pub struct BlockId(pub CoreBlockId);

type Finalized = bool;

impl BlockId {
    pub fn from_slot(slot: Slot) -> Self {
        Self(CoreBlockId::Slot(slot))
    }

    pub fn from_root(root: Hash256) -> Self {
        Self(CoreBlockId::Root(root))
    }

    /// Return the block root identified by `self`.
    pub fn root<T: BeaconChainTypes>(
        &self,
        chain: &BeaconChain<T>,
    ) -> Result<(Hash256, ExecutionOptimistic, Finalized), warp::Rejection> {
        match &self.0 {
            CoreBlockId::Head => {
                let (cached_head, execution_status) = chain
                    .canonical_head
                    .head_and_execution_status()
                    .map_err(warp_utils::reject::beacon_chain_error)?;
                Ok((
                    cached_head.head_block_root(),
                    execution_status.is_optimistic_or_invalid(),
                    false,
                ))
            }
            CoreBlockId::Genesis => Ok((chain.genesis_block_root, false, true)),
            CoreBlockId::Finalized => {
                let finalized_checkpoint =
                    chain.canonical_head.cached_head().finalized_checkpoint();
                let (_slot, execution_optimistic) =
                    checkpoint_slot_and_execution_optimistic(chain, finalized_checkpoint)?;
                Ok((finalized_checkpoint.root, execution_optimistic, true))
            }
            CoreBlockId::Justified => {
                let justified_checkpoint =
                    chain.canonical_head.cached_head().justified_checkpoint();
                let (_slot, execution_optimistic) =
                    checkpoint_slot_and_execution_optimistic(chain, justified_checkpoint)?;
                Ok((justified_checkpoint.root, execution_optimistic, false))
            }
            CoreBlockId::Slot(slot) => {
                let execution_optimistic = chain
                    .is_optimistic_or_invalid_head()
                    .map_err(warp_utils::reject::beacon_chain_error)?;
                let root = chain
                    .block_root_at_slot(*slot, WhenSlotSkipped::None)
                    .map_err(warp_utils::reject::beacon_chain_error)
                    .and_then(|root_opt| {
                        root_opt.ok_or_else(|| {
                            warp_utils::reject::custom_not_found(format!(
                                "beacon block at slot {}",
                                slot
                            ))
                        })
                    })?;
                let finalized = *slot
                    <= chain
                        .canonical_head
                        .cached_head()
                        .finalized_checkpoint()
                        .epoch
                        .start_slot(T::EthSpec::slots_per_epoch());
                Ok((root, execution_optimistic, finalized))
            }
            CoreBlockId::Root(root) => {
                // This matches the behaviour of other consensus clients (e.g. Teku).
                if root == &Hash256::zero() {
                    return Err(warp_utils::reject::custom_not_found(format!(
                        "beacon block with root {}",
                        root
                    )));
                };
                if chain
                    .store
                    .block_exists(root)
                    .map_err(BeaconChainError::DBError)
                    .map_err(warp_utils::reject::beacon_chain_error)?
                {
                    let execution_optimistic = chain
                        .canonical_head
                        .fork_choice_read_lock()
                        .is_optimistic_or_invalid_block(root)
                        .map_err(BeaconChainError::ForkChoiceError)
                        .map_err(warp_utils::reject::beacon_chain_error)?;
                    let blinded_block = chain
                        .get_blinded_block(root)
                        .map_err(warp_utils::reject::beacon_chain_error)?
                        .ok_or_else(|| {
                            warp_utils::reject::custom_not_found(format!(
                                "beacon block with root {}",
                                root
                            ))
                        })?;
                    let block_slot = blinded_block.slot();
                    let finalized = chain
                        .is_finalized_block(root, block_slot)
                        .map_err(warp_utils::reject::beacon_chain_error)?;
                    Ok((*root, execution_optimistic, finalized))
                } else {
                    Err(warp_utils::reject::custom_not_found(format!(
                        "beacon block with root {}",
                        root
                    )))
                }
            }
        }
    }

    /// Return the `SignedBeaconBlock` identified by `self`.
    pub fn blinded_block<T: BeaconChainTypes>(
        &self,
        chain: &BeaconChain<T>,
    ) -> Result<
        (
            SignedBlindedBeaconBlock<T::EthSpec>,
            ExecutionOptimistic,
            Finalized,
        ),
        warp::Rejection,
    > {
        match &self.0 {
            CoreBlockId::Head => {
                let (cached_head, execution_status) = chain
                    .canonical_head
                    .head_and_execution_status()
                    .map_err(warp_utils::reject::beacon_chain_error)?;
                Ok((
                    cached_head.snapshot.beacon_block.clone_as_blinded(),
                    execution_status.is_optimistic_or_invalid(),
                    false,
                ))
            }
            CoreBlockId::Slot(slot) => {
                let (root, execution_optimistic, finalized) = self.root(chain)?;
                chain
                    .get_blinded_block(&root)
                    .map_err(warp_utils::reject::beacon_chain_error)
                    .and_then(|block_opt| match block_opt {
                        Some(block) => {
                            if block.slot() != *slot {
                                return Err(warp_utils::reject::custom_not_found(format!(
                                    "slot {} was skipped",
                                    slot
                                )));
                            }
                            Ok((block, execution_optimistic, finalized))
                        }
                        None => Err(warp_utils::reject::custom_not_found(format!(
                            "beacon block with root {}",
                            root
                        ))),
                    })
            }
            _ => {
                let (root, execution_optimistic, finalized) = self.root(chain)?;
                let block = chain
                    .get_blinded_block(&root)
                    .map_err(warp_utils::reject::beacon_chain_error)
                    .and_then(|root_opt| {
                        root_opt.ok_or_else(|| {
                            warp_utils::reject::custom_not_found(format!(
                                "beacon block with root {}",
                                root
                            ))
                        })
                    })?;
                Ok((block, execution_optimistic, finalized))
            }
        }
    }

    /// Return the `SignedBeaconBlock` identified by `self`.
    pub async fn full_block<T: BeaconChainTypes>(
        &self,
        chain: &BeaconChain<T>,
    ) -> Result<
        (
            Arc<SignedBeaconBlock<T::EthSpec>>,
            ExecutionOptimistic,
            Finalized,
        ),
        warp::Rejection,
    > {
        match &self.0 {
            CoreBlockId::Head => {
                let (cached_head, execution_status) = chain
                    .canonical_head
                    .head_and_execution_status()
                    .map_err(warp_utils::reject::beacon_chain_error)?;
                Ok((
                    cached_head.snapshot.beacon_block.clone(),
                    execution_status.is_optimistic_or_invalid(),
                    false,
                ))
            }
            CoreBlockId::Slot(slot) => {
                let (root, execution_optimistic, finalized) = self.root(chain)?;
                chain
                    .get_block(&root)
                    .await
                    .map_err(warp_utils::reject::beacon_chain_error)
                    .and_then(|block_opt| match block_opt {
                        Some(block) => {
                            if block.slot() != *slot {
                                return Err(warp_utils::reject::custom_not_found(format!(
                                    "slot {} was skipped",
                                    slot
                                )));
                            }
                            Ok((Arc::new(block), execution_optimistic, finalized))
                        }
                        None => Err(warp_utils::reject::custom_not_found(format!(
                            "beacon block with root {}",
                            root
                        ))),
                    })
            }
            _ => {
                let (root, execution_optimistic, finalized) = self.root(chain)?;
                chain
                    .get_block(&root)
                    .await
                    .map_err(warp_utils::reject::beacon_chain_error)
                    .and_then(|block_opt| {
                        block_opt
                            .map(|block| (Arc::new(block), execution_optimistic, finalized))
                            .ok_or_else(|| {
                                warp_utils::reject::custom_not_found(format!(
                                    "beacon block with root {}",
                                    root
                                ))
                            })
                    })
            }
        }
    }

    /// Return the `BlobSidecarList` identified by `self`.
    pub fn blob_sidecar_list<T: BeaconChainTypes>(
        &self,
        chain: &BeaconChain<T>,
    ) -> Result<BlobSidecarList<T::EthSpec>, warp::Rejection> {
        let root = self.root(chain)?.0;
        chain
            .get_blobs(&root)
            .map_err(warp_utils::reject::beacon_chain_error)
    }

    pub fn blob_sidecar_list_filtered<T: BeaconChainTypes>(
        &self,
        indices: BlobIndicesQuery,
        chain: &BeaconChain<T>,
    ) -> Result<BlobSidecarList<T::EthSpec>, warp::Rejection> {
        let blob_sidecar_list = self.blob_sidecar_list(chain)?;
        let blob_sidecar_list_filtered = match indices.indices {
            Some(vec) => {
                let list = blob_sidecar_list
                    .into_iter()
                    .filter(|blob_sidecar| vec.contains(&blob_sidecar.index))
                    .collect();
                BlobSidecarList::new(list)
                    .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))?
            }
            None => blob_sidecar_list,
        };
        Ok(blob_sidecar_list_filtered)
    }

    pub fn merkle_brunch_root<T: BeaconChainTypes>(
        &self,
        chain: &BeaconChain<T>,
    ) -> Result<(Hash256, Vec<Hash256>), warp::Rejection> {
        match &self.0 {
            eth2::types::BlockId::Slot(slot) => {
                let epoch = slot.epoch(T::EthSpec::slots_per_epoch());
                let index = epoch
                    .position(*slot, T::EthSpec::slots_per_epoch())
                    .unwrap();

                let block_roots_iter = chain
                    .forwards_iter_block_roots_until(
                        epoch.start_slot(T::EthSpec::slots_per_epoch()),
                        epoch.end_slot(T::EthSpec::slots_per_epoch()),
                    )
                    .unwrap();

                let block_roots = block_roots_iter.map(|e| e.unwrap().0).collect::<Vec<_>>();

                let depth = T::EthSpec::slots_per_epoch().ilog2() as usize;
                let merkle_tree = MerkleTree::create(&block_roots, depth);

                merkle_tree
                    .generate_proof(index, depth)
                    .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))
            }
            _ => todo!(),
        }
    }

    // pub async fn block_body_merkle_proof<T: BeaconChainTypes>(
    //     &self,
    //     chain: &BeaconChain<T>,
    // ) -> Result<DeepStorageBlock<E>, warp::Rejection> {
    //     let block_id = self.clone();
    //     let (block_root, block_root_branch) = block_id.merkle_brunch_root(&chain)?;

    //     let (block, _, _) = block_id.full_block(&chain).await?;

    //     let beacon_block_body = BeaconBlockBody::from(
    //         block
    //             .message()
    //             .body_capella()
    //             .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))?
    //             .to_owned(),
    //     );

    //     let execution = block
    //         .message()
    //         .execution_payload()
    //         .map_err(|e| warp_utils::reject::custom_server_error(format!("{:?}", e)))?
    //         .to_execution_payload_header();
        
    //     let execution_branch = beacon_block_body
    //         .block_body_merkle_proof(EXECUTION_PAYLOAD_INDEX)
    //         .unwrap();

    //     let fork_name = block
    //         .fork_name(&chain.spec)
    //         .map_err(inconsistent_fork_rejection)?;

    //     let data = match fork_name {
    //         // ForkName::Altair | ForkName::Merge => {
    //         //     let header = LightClientHeaderAltair::from_ssz_bytes(bytes)?;
    //         //     LightClientHeader::Altair(header)
    //         // }
    //         ForkName::Capella => DeepStorageBlock::Capella(DeepStorageBlockCapella {
    //             block_root_branch,
    //             execution: *execution.as_capella().unwrap(),
    //             execution_branch,
    //             _phantom_data: std::marker::PhantomData,
    //         }),
    //         // ForkName::Deneb => {
    //         //     let header = LightClientHeaderDeneb::from_ssz_bytes(bytes)?;
    //         //     LightClientHeader::Deneb(header)
    //         // }
    //         // ForkName::Base => {
    //         //     return Err(ssz::DecodeError::BytesInvalid(format!(
    //         //         "LightClientHeader decoding for {fork_name} not implemented"
    //         //     )))
    //         // }
    //         _ => todo!(),
    //     };

    //     Ok(data)
    // }
}

impl FromStr for BlockId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CoreBlockId::from_str(s).map(Self)
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
