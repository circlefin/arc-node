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

// adapted from https://github.com/informalsystems/malachite/tree/v0.4.0/code/crates/test
use bytes::Bytes;
use malachitebft_core_types::Round;
use malachitebft_proto::{Error as ProtoError, Protobuf};
use ssz::{Decode, Encode};
use ssz_derive::{Decode, Encode};

use crate::{Address, ArcContext, Height, Value};

use crate::ssz::round as ssz_round;

/// A proposal for a value in a round
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct Proposal {
    pub height: Height,
    #[ssz(with = "ssz_round")]
    pub round: Round,
    pub value: Value,
    #[ssz(with = "ssz_round")]
    pub pol_round: Round,
    pub validator_address: Address,
}

impl Proposal {
    pub fn new(
        height: Height,
        round: Round,
        value: Value,
        pol_round: Round,
        validator_address: Address,
    ) -> Self {
        Self {
            height,
            round,
            value,
            pol_round,
            validator_address,
        }
    }

    pub fn to_sign_bytes(&self) -> Bytes {
        // It's an invariant that a proposal has a non-nil round.
        // A panic here indicates a bug in proposal creation logic.
        assert!(self.round.is_defined(), "round should not be nil");

        self.as_ssz_bytes().into()
    }

    pub fn from_sign_bytes(bytes: &[u8]) -> Result<Self, ssz::DecodeError> {
        Proposal::from_ssz_bytes(bytes)
    }
}

impl malachitebft_core_types::Proposal<ArcContext> for Proposal {
    fn height(&self) -> Height {
        self.height
    }

    fn round(&self) -> Round {
        self.round
    }

    fn value(&self) -> &Value {
        &self.value
    }

    fn take_value(self) -> Value {
        self.value
    }

    fn pol_round(&self) -> Round {
        self.pol_round
    }

    fn validator_address(&self) -> &Address {
        &self.validator_address
    }
}

impl Protobuf for Proposal {
    type Proto = crate::proto::Proposal;

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(Self::Proto {
            height: self.height.to_proto()?,
            round: self.round.as_u32().expect("round should not be nil"),
            value: Some(self.value.to_proto()?),
            pol_round: self.pol_round.as_u32(),
            validator_address: Some(self.validator_address.to_proto()?),
        })
    }

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        Ok(Self {
            height: Height::from_proto(proto.height)?,
            round: Round::new(proto.round),
            value: Value::from_proto(
                proto
                    .value
                    .ok_or_else(|| ProtoError::missing_field::<Self::Proto>("value"))?,
            )?,
            pol_round: Round::from(proto.pol_round),
            validator_address: Address::from_proto(
                proto
                    .validator_address
                    .ok_or_else(|| ProtoError::missing_field::<Self::Proto>("validator_address"))?,
            )?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proposal_ssz_encode_decode() {
        use alloy_primitives::BlockHash;

        let height = Height::new(100);
        let round = Round::new(5);
        let pol_round = Round::new(3);
        let block_hash = BlockHash::new([0xAA; 32]);
        let value = Value::new(block_hash);
        let validator_address = Address::new([0xBB; 20]);

        // Test proposal with defined pol_round
        let proposal = Proposal::new(height, round, value.clone(), pol_round, validator_address);

        // Test SSZ encoding
        let ssz_bytes = proposal.as_ssz_bytes();
        assert!(!ssz_bytes.is_empty());
        assert_eq!(ssz_bytes.len(), proposal.ssz_bytes_len());

        // Test SSZ decoding
        let decoded_proposal = Proposal::from_ssz_bytes(&ssz_bytes).unwrap();
        assert_eq!(proposal.height, decoded_proposal.height);
        assert_eq!(proposal.round, decoded_proposal.round);
        assert_eq!(proposal.value, decoded_proposal.value);
        assert_eq!(proposal.pol_round, decoded_proposal.pol_round);
        assert_eq!(
            proposal.validator_address,
            decoded_proposal.validator_address
        );

        // Test with Nil pol_round
        let nil_pol_round = Round::Nil;
        let nil_proposal = Proposal::new(height, round, value, nil_pol_round, validator_address);

        let ssz_bytes = nil_proposal.as_ssz_bytes();
        let decoded_nil_proposal = Proposal::from_ssz_bytes(&ssz_bytes).unwrap();
        assert_eq!(nil_proposal.pol_round, decoded_nil_proposal.pol_round);

        assert!(!<Proposal as ssz::Encode>::is_ssz_fixed_len());
    }

    /// Signed preimages of different consensus message types must never collide.
    ///
    /// `Vote::to_sign_bytes` and `Proposal::to_sign_bytes` both hand raw SSZ to the
    /// signer with no domain separator, unlike `ValidatorProof::signing_bytes`, which
    /// prefixes `b"PoV"`. Today the two cannot collide because their encoded lengths are
    /// disjoint -- `Vote` has one variable field pair and `Proposal` has two, so their SSZ
    /// offset tables differ in size. That is an emergent property of the current field
    /// sets, not an enforced one: adding a fixed field to `Vote`, or making `Value`
    /// variable-length, could make the sets overlap and silently permit a signature over a
    /// vote to be replayed as a proposal.
    ///
    /// This test pins the invariant so such a change fails loudly instead.
    #[test]
    fn test_vote_and_proposal_sign_bytes_lengths_are_disjoint() {
        use alloy_primitives::BlockHash;

        use crate::{Vote, ValueId};
        use malachitebft_core_types::NilOrVal;

        let height = Height::new(100);
        let round = Round::new(5);
        let block_hash = BlockHash::new([0xAA; 32]);
        let validator_address = Address::new([0xBB; 20]);

        // `to_sign_bytes` asserts the round is defined, so only `pol_round` / `value` vary.
        let vote_lens: Vec<usize> = [NilOrVal::Nil, NilOrVal::Val(ValueId::new(block_hash))]
            .into_iter()
            .map(|value| {
                Vote::new_prevote(height, round, value, validator_address)
                    .to_sign_bytes()
                    .len()
            })
            .collect();

        let proposal_lens: Vec<usize> = [Round::Nil, Round::new(3)]
            .into_iter()
            .map(|pol_round| {
                Proposal::new(
                    height,
                    round,
                    Value::new(block_hash),
                    pol_round,
                    validator_address,
                )
                .to_sign_bytes()
                .len()
            })
            .collect();

        for v in &vote_lens {
            assert!(
                !proposal_lens.contains(v),
                "Vote and Proposal signed-byte lengths overlap at {v} bytes \
                 (vote={vote_lens:?}, proposal={proposal_lens:?}). Cross-type signature \
                 replay may be possible; add an explicit domain separator to \
                 `to_sign_bytes` for both types."
            );
        }
    }

}
