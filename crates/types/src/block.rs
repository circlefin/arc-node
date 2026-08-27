// Copyright 2025 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloy_eips::eip7685::Requests;
use alloy_rpc_types_engine::{
    CancunPayloadFields, ExecutionData, ExecutionPayload, ExecutionPayloadSidecar,
    ExecutionPayloadV3, PayloadError, PraguePayloadFields,
};
use bytesize::ByteSize;
use ssz::Encode;

use malachitebft_app_channel::app::types::core::{CommitCertificate, Round, Validity};
use malachitebft_app_channel::app::types::{LocallyProposedValue, ProposedValue};

use crate::ssz::{SszBlock, SszSignature};
use crate::{signing::Signature, Address, ArcContext, BlockHash, Height, Value};

/// A block as seen by the consensus layer.
///
/// This includes the execution payload, the metadata required for consensus,
/// and the signature for its proposal parts.
/// Note that this is a block that has been proposed but not yet decided, that is,
/// consensus has not yet been reached on it. Therefore, it might not become the
/// next head of the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsensusBlock {
    pub height: Height,
    pub round: Round,
    pub valid_round: Round,
    pub proposer: Address,
    pub validity: Validity,
    pub execution_payload: ExecutionPayloadV3,
    pub signature: Option<Signature>,
}

impl ConsensusBlock {
    /// Returns the block hash carried verbatim in the execution payload.
    ///
    /// Not recomputed from the payload, so it is untrusted; use
    /// [`Self::canonical_block_hash`] where a verified hash is required.
    pub fn self_reported_block_hash(&self) -> BlockHash {
        self.execution_payload
            .payload_inner
            .payload_inner
            .block_hash
    }

    /// Recomputes the canonical block hash from the execution payload contents.
    pub fn canonical_block_hash(&self) -> Result<BlockHash, PayloadError> {
        canonical_block_hash(&self.execution_payload)
    }

    /// Returns whether the self-reported block hash matches the hash recomputed
    /// from the payload contents.
    pub fn self_reported_hash_is_canonical(&self) -> bool {
        self.canonical_block_hash()
            .is_ok_and(|canonical| canonical == self.self_reported_block_hash())
    }

    /// Returns whether this block may be keyed into the undecided-blocks table:
    /// valid blocks always may; invalid blocks only if their self-reported hash
    /// is canonical.
    pub fn may_be_stored_as_undecided(&self) -> bool {
        self.validity.is_valid() || self.self_reported_hash_is_canonical()
    }

    /// Returns the size of the block in bytes when encoded using SSZ.
    pub fn size_bytes(&self) -> ByteSize {
        // TODO: Cache this
        ByteSize::b(block_as_ssz_data(self).ssz_bytes_len() as u64)
    }

    /// Returns the size of the execution payload in bytes when encoded using SSZ.
    pub fn payload_size(&self) -> ByteSize {
        ByteSize::b(self.execution_payload.ssz_bytes_len() as u64)
    }

    /// Builds the [`ProposedValue`] voted on for this block, using `validity` for
    /// the vote instead of the block's persisted [`Self::validity`].
    ///
    /// The value id is always the self-reported (wire) hash peers vote on; only
    /// the vote validity is overridden, so a caller can prevote nil on a block it
    /// still stores with a different (execution-only) validity.
    pub fn to_proposed_value_with_validity(&self, validity: Validity) -> ProposedValue<ArcContext> {
        ProposedValue {
            height: self.height,
            round: self.round,
            proposer: self.proposer,
            valid_round: self.valid_round,
            value: Value::new(self.self_reported_block_hash()),
            validity,
        }
    }
}

// The value id is the self-reported (wire) hash that peers vote on.
impl From<&ConsensusBlock> for ProposedValue<ArcContext> {
    fn from(block: &ConsensusBlock) -> Self {
        block.to_proposed_value_with_validity(block.validity)
    }
}

impl From<&ConsensusBlock> for LocallyProposedValue<ArcContext> {
    fn from(block: &ConsensusBlock) -> Self {
        LocallyProposedValue {
            height: block.height,
            round: block.round,
            value: Value::new(block.self_reported_block_hash()),
        }
    }
}

/// Recomputes the canonical block hash of an execution payload from its
/// contents.
///
/// Arc has no beacon chain and no execution-layer requests: the
/// `parent_beacon_block_root` is the parent block hash, and the Prague requests
/// list is always empty. The block is reconstructed through the same
/// sidecar-aware path the execution layer uses, so the hash always matches the
/// one the engine validates in `engine_newPayloadV3`.
///
/// Clones the payload to reconstruct the block; keep it off the hot path.
pub fn canonical_block_hash(payload: &ExecutionPayloadV3) -> Result<BlockHash, PayloadError> {
    let parent_beacon_block_root = payload.payload_inner.payload_inner.parent_hash;
    let sidecar = ExecutionPayloadSidecar::v4(
        CancunPayloadFields::new(parent_beacon_block_root, vec![]),
        PraguePayloadFields::new(Requests::default()),
    );
    let block =
        ExecutionData::new(ExecutionPayload::V3(payload.clone()), sidecar).into_block_raw()?;
    Ok(block.header.hash_slow())
}

/// Converts a ConsensusBlock into a tuple suitable for SSZ encoding
pub fn block_as_ssz_data(block: &ConsensusBlock) -> SszBlock<&'_ ExecutionPayloadV3> {
    (
        block.height.as_u64(),
        block.round.as_u32(),
        block.valid_round.as_u32(),
        block.proposer.to_alloy_address(),
        block.validity.is_valid(),
        &block.execution_payload,
        block.signature.map(SszSignature),
    )
}

/// Decided block, ie. an execution payload together with its commit certificate.
/// A decided block is a block for which consensus has been reached and therefore
/// it can't be removed from the chain.
#[derive(Clone, Debug)]
pub struct DecidedBlock {
    pub execution_payload: ExecutionPayloadV3,
    pub certificate: CommitCertificate<ArcContext>,
}

impl DecidedBlock {
    /// Creates a new decided block from an execution payload and a commit certificate.
    /// The block hash in the execution payload must match the hash in the commit certificate.
    pub fn new(
        execution_payload: ExecutionPayloadV3,
        certificate: CommitCertificate<ArcContext>,
    ) -> Self {
        let payload_block_hash = execution_payload.payload_inner.payload_inner.block_hash;
        let certificate_block_hash = certificate.value_id.block_hash();

        assert_eq!(
            payload_block_hash, certificate_block_hash,
            "Block hash in the execution payload does not match the hash in the commit certificate"
        );

        Self {
            execution_payload,
            certificate,
        }
    }

    /// Returns the height at which the block was decided.
    pub fn height(&self) -> Height {
        self.certificate.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Block, B256};

    fn block_with_self_reported_hash(self_reported: BlockHash) -> ConsensusBlock {
        let inner: Block = Block::default();
        let mut payload = ExecutionPayloadV3::from_block_unchecked(self_reported, &inner);
        payload.payload_inner.payload_inner.block_hash = self_reported;

        ConsensusBlock {
            height: Height::new(1),
            round: Round::new(0),
            valid_round: Round::Nil,
            proposer: Address::new([0u8; 20]),
            validity: Validity::Valid,
            execution_payload: payload,
            signature: None,
        }
    }

    #[test]
    fn canonical_block_hash_is_independent_of_self_reported_hash() {
        let canonical = block_with_self_reported_hash(B256::ZERO)
            .canonical_block_hash()
            .expect("recompute canonical hash");

        // The canonical hash is derived from the payload contents, so it is the
        // same regardless of which value the payload claims as its block hash.
        let other = block_with_self_reported_hash(B256::repeat_byte(0xAB))
            .canonical_block_hash()
            .expect("recompute canonical hash");

        assert_eq!(canonical, other);
    }

    #[test]
    fn self_reported_hash_is_canonical_only_when_it_matches_contents() {
        let canonical = block_with_self_reported_hash(B256::ZERO)
            .canonical_block_hash()
            .expect("recompute canonical hash");

        let matching = block_with_self_reported_hash(canonical);
        assert_eq!(matching.self_reported_block_hash(), canonical);
        assert!(matching.self_reported_hash_is_canonical());

        let mismatched = block_with_self_reported_hash(B256::repeat_byte(0x99));
        assert!(!mismatched.self_reported_hash_is_canonical());
        assert_eq!(
            mismatched.canonical_block_hash().expect("recompute"),
            canonical
        );
    }

    #[test]
    fn may_be_stored_as_undecided_rejects_only_invalid_non_canonical_blocks() {
        let canonical = block_with_self_reported_hash(B256::ZERO)
            .canonical_block_hash()
            .expect("recompute canonical hash");

        let mut valid_non_canonical = block_with_self_reported_hash(B256::repeat_byte(0x99));
        valid_non_canonical.validity = Validity::Valid;
        assert!(valid_non_canonical.may_be_stored_as_undecided());

        let mut invalid_canonical = block_with_self_reported_hash(canonical);
        invalid_canonical.validity = Validity::Invalid;
        assert!(invalid_canonical.may_be_stored_as_undecided());

        let mut invalid_non_canonical = block_with_self_reported_hash(B256::repeat_byte(0x99));
        invalid_non_canonical.validity = Validity::Invalid;
        assert!(!invalid_non_canonical.may_be_stored_as_undecided());
    }

    #[test]
    fn to_proposed_value_with_validity_overrides_only_the_vote_validity() {
        let mut block = block_with_self_reported_hash(B256::repeat_byte(0x7));
        block.validity = Validity::Valid;

        let voted = block.to_proposed_value_with_validity(Validity::Invalid);

        // The vote validity is overridden, while the block's persisted validity
        // is untouched and the value id + metadata still match the block.
        assert_eq!(voted.validity, Validity::Invalid);
        assert_eq!(block.validity, Validity::Valid);
        assert_eq!(voted.value, Value::new(block.self_reported_block_hash()));
        assert_eq!(voted.height, block.height);
        assert_eq!(voted.round, block.round);
        assert_eq!(voted.proposer, block.proposer);
        assert_eq!(voted.valid_round, block.valid_round);

        // The `From` impl keeps deriving the vote validity from the block.
        assert_eq!(ProposedValue::from(&block).validity, Validity::Valid);
    }

    #[test]
    fn canonical_block_hash_includes_prague_requests_hash() {
        // Arc runs Prague with no execution requests, so the execution layer
        // seals headers with the empty-requests hash and the parent hash as the
        // beacon root. The canonical hash must match that reconstruction, not
        // the requests_hash = None one that `into_block_raw` produces on its own.
        let payload = block_with_self_reported_hash(B256::ZERO).execution_payload;

        let mut expected = payload.clone().into_block_raw().expect("into_block_raw");
        expected.header.parent_beacon_block_root =
            Some(payload.payload_inner.payload_inner.parent_hash);
        expected.header.requests_hash = Some(alloy_eips::eip7685::EMPTY_REQUESTS_HASH);

        assert_eq!(
            canonical_block_hash(&payload).expect("canonical hash"),
            expected.header.hash_slow(),
        );
    }
}
