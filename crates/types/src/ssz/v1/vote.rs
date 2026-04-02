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

use ssz_derive::{Decode, Encode};

use crate::{ArcContext, Vote};
use malachitebft_core_types::{SignedVote, VoteType};
use malachitebft_signing_ed25519::Signature;

/// SSZ encoding of a vote type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[ssz(enum_behaviour = "tag")]
pub enum SszVoteType {
    Prevote,
    Precommit,
}

impl From<VoteType> for SszVoteType {
    fn from(vote_type: VoteType) -> Self {
        match vote_type {
            VoteType::Prevote => SszVoteType::Prevote,
            VoteType::Precommit => SszVoteType::Precommit,
        }
    }
}

impl From<SszVoteType> for VoteType {
    fn from(vote_type: SszVoteType) -> Self {
        match vote_type {
            SszVoteType::Prevote => VoteType::Prevote,
            SszVoteType::Precommit => VoteType::Precommit,
        }
    }
}

pub mod vote_type {
    use super::*;

    pub mod encode {
        use super::*;
        use ssz::Encode;

        pub fn is_ssz_fixed_len() -> bool {
            <SszVoteType as Encode>::is_ssz_fixed_len()
        }

        pub fn ssz_fixed_len() -> usize {
            <SszVoteType as Encode>::ssz_fixed_len()
        }

        pub fn ssz_bytes_len(vote_type: &VoteType) -> usize {
            <SszVoteType as Encode>::ssz_bytes_len(&SszVoteType::from(*vote_type))
        }

        pub fn ssz_append(vote_type: &VoteType, buf: &mut Vec<u8>) {
            SszVoteType::from(*vote_type).ssz_append(buf)
        }
    }

    pub mod decode {
        use super::*;
        use ssz::Decode;

        pub fn is_ssz_fixed_len() -> bool {
            <SszVoteType as Decode>::is_ssz_fixed_len()
        }

        pub fn ssz_fixed_len() -> usize {
            <SszVoteType as Decode>::ssz_fixed_len()
        }

        pub fn from_ssz_bytes(bytes: &[u8]) -> Result<VoteType, ssz::DecodeError> {
            SszVoteType::from_ssz_bytes(bytes).map(|v| v.into())
        }
    }
}

const SIGNATURE_SIZE: usize = 64;

/// SSZ encoding of a signed vote.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
pub struct SszSignedVote {
    pub signature: [u8; SIGNATURE_SIZE],
    pub vote: Vote,
}

impl From<SignedVote<ArcContext>> for SszSignedVote {
    fn from(signed_vote: SignedVote<ArcContext>) -> Self {
        SszSignedVote {
            vote: signed_vote.message,
            signature: signed_vote.signature.to_bytes(),
        }
    }
}

impl From<SszSignedVote> for SignedVote<ArcContext> {
    fn from(ssz_signed_vote: SszSignedVote) -> Self {
        SignedVote::new(
            ssz_signed_vote.vote,
            Signature::from_bytes(ssz_signed_vote.signature),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssz::{Decode, Encode};

    #[test]
    fn test_ssz_vote_type_encode_decode() {
        let vote_type = VoteType::Prevote;
        let ssz_vote_type = SszVoteType::from(vote_type);
        let encoded = ssz_vote_type.as_ssz_bytes();
        let decoded: VoteType = SszVoteType::from_ssz_bytes(&encoded).unwrap().into();
        assert_eq!(vote_type, decoded);
        assert!(<SszVoteType as ssz::Encode>::is_ssz_fixed_len());
    }
}
