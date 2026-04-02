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

use bytes::Bytes;
use malachitebft_core_types::{NilOrVal, Round, SignedExtension, VoteType};
use malachitebft_proto::{Error as ProtoError, Protobuf};
use ssz::{Decode, Encode};
use ssz_derive::{Decode, Encode};

use crate::proto;
use crate::{Address, ArcContext, Height, ValueId};

pub use malachitebft_core_types::Extension;

use crate::ssz::nil_or_val as ssz_nil_or_val;
use crate::ssz::round as ssz_round;
use crate::ssz::vote::vote_type as ssz_vote_type;

/// A vote for a value in a round
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Encode, Decode)]
pub struct Vote {
    #[ssz(with = "ssz_vote_type")]
    pub typ: VoteType,
    pub height: Height,
    #[ssz(with = "ssz_round")]
    pub round: Round,
    #[ssz(with = "ssz_nil_or_val")]
    pub value: NilOrVal<ValueId>,
    pub validator_address: Address,
    #[ssz(skip_serializing, skip_deserializing)]
    pub extension: Option<SignedExtension<ArcContext>>,
}

impl Vote {
    /// Create a new prevote.
    pub fn new_prevote(
        height: Height,
        round: Round,
        value: NilOrVal<ValueId>,
        validator_address: Address,
    ) -> Self {
        Self {
            typ: VoteType::Prevote,
            height,
            round,
            value,
            validator_address,
            extension: None,
        }
    }

    /// Create a new precommit.
    pub fn new_precommit(
        height: Height,
        round: Round,
        value: NilOrVal<ValueId>,
        address: Address,
    ) -> Self {
        Self {
            typ: VoteType::Precommit,
            height,
            round,
            value,
            validator_address: address,
            extension: None,
        }
    }

    /// Convert the vote to bytes that can be signed.
    pub fn to_sign_bytes(&self) -> Bytes {
        // It's an invariant that a vote has a non-nil round.
        // A panic here indicates a bug in vote creation logic.
        assert!(self.round.is_defined(), "round should not be nil");

        let vote = Self {
            extension: None,
            ..self.clone()
        };

        vote.as_ssz_bytes().into()
    }

    /// Decode a vote from the signed bytes.
    pub fn from_sign_bytes(bytes: &[u8]) -> Result<Self, ssz::DecodeError> {
        Vote::from_ssz_bytes(bytes)
    }
}

impl malachitebft_core_types::Vote<ArcContext> for Vote {
    fn height(&self) -> Height {
        self.height
    }

    fn round(&self) -> Round {
        self.round
    }

    fn value(&self) -> &NilOrVal<ValueId> {
        &self.value
    }

    fn take_value(self) -> NilOrVal<ValueId> {
        self.value
    }

    fn vote_type(&self) -> VoteType {
        self.typ
    }

    fn validator_address(&self) -> &Address {
        &self.validator_address
    }

    fn extension(&self) -> Option<&SignedExtension<ArcContext>> {
        self.extension.as_ref()
    }

    fn take_extension(&mut self) -> Option<SignedExtension<ArcContext>> {
        self.extension.take()
    }

    fn extend(self, extension: SignedExtension<ArcContext>) -> Self {
        Self {
            extension: Some(extension),
            ..self
        }
    }
}

impl Protobuf for Vote {
    type Proto = crate::proto::Vote;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        Ok(Self {
            typ: decode_votetype(proto.vote_type()),
            height: Height::from_proto(proto.height)?,
            round: Round::new(proto.round),
            value: match proto.value {
                Some(value) => NilOrVal::Val(ValueId::from_proto(value)?),
                None => NilOrVal::Nil,
            },
            validator_address: Address::from_proto(
                proto
                    .validator_address
                    .ok_or_else(|| ProtoError::missing_field::<Self::Proto>("validator_address"))?,
            )?,
            extension: Default::default(),
        })
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(Self::Proto {
            vote_type: encode_votetype(self.typ).into(),
            height: self.height.to_proto()?,
            round: self.round.as_u32().expect("round should not be nil"),
            value: match &self.value {
                NilOrVal::Nil => None,
                NilOrVal::Val(v) => Some(v.to_proto()?),
            },
            validator_address: Some(self.validator_address.to_proto()?),
        })
    }
}

pub fn encode_votetype(vote_type: VoteType) -> proto::VoteType {
    match vote_type {
        VoteType::Prevote => proto::VoteType::Prevote,
        VoteType::Precommit => proto::VoteType::Precommit,
    }
}

pub fn decode_votetype(vote_type: proto::VoteType) -> VoteType {
    match vote_type {
        proto::VoteType::Prevote => VoteType::Prevote,
        proto::VoteType::Precommit => VoteType::Precommit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vote_ssz_encode_decode() {
        use alloy_primitives::BlockHash;

        let height = Height::new(100);
        let round = Round::new(5);
        let block_hash = BlockHash::new([0xAA; 32]);
        let value = NilOrVal::Val(ValueId::new(block_hash));

        let validator_address = Address::new([0xBB; 20]);

        // Test non-nil prevote
        let prevote = Vote::new_prevote(height, round, value, validator_address);

        // Test SSZ encoding
        let ssz_bytes = prevote.as_ssz_bytes();
        assert!(!ssz_bytes.is_empty());
        assert_eq!(ssz_bytes.len(), prevote.ssz_bytes_len());

        // Test SSZ decoding
        let decoded_prevote = Vote::from_ssz_bytes(&ssz_bytes).unwrap();
        assert_eq!(prevote.typ, decoded_prevote.typ);
        assert_eq!(prevote.height, decoded_prevote.height);
        assert_eq!(prevote.round, decoded_prevote.round);
        assert_eq!(prevote.value, decoded_prevote.value);
        assert_eq!(prevote.validator_address, decoded_prevote.validator_address);
        assert_eq!(prevote.extension, decoded_prevote.extension);

        // Test with Nil value
        let nil_value = NilOrVal::Nil;
        let nil_vote = Vote::new_prevote(height, round, nil_value, validator_address);

        let ssz_bytes = nil_vote.as_ssz_bytes();
        let decoded_nil_vote = Vote::from_ssz_bytes(&ssz_bytes).unwrap();
        assert_eq!(nil_vote.value, decoded_nil_vote.value);

        assert!(!<Vote as ssz::Encode>::is_ssz_fixed_len());
    }
}
