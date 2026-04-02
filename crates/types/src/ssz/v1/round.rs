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

use malachitebft_core_types::Round;

/// SSZ encoding of a round.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Encode, Decode)]
#[ssz(struct_behaviour = "transparent")]
pub struct SszRound(Option<u32>);

impl From<Round> for SszRound {
    fn from(round: Round) -> Self {
        match round {
            Round::Nil => SszRound(None),
            Round::Some(round) => SszRound(Some(round)),
        }
    }
}

impl From<SszRound> for Round {
    fn from(round: SszRound) -> Self {
        match round {
            SszRound(Some(round)) => Round::new(round),
            SszRound(None) => Round::Nil,
        }
    }
}

pub mod encode {
    use ssz::Encode;

    use super::*;

    pub fn is_ssz_fixed_len() -> bool {
        <SszRound as Encode>::is_ssz_fixed_len()
    }

    pub fn ssz_fixed_len() -> usize {
        <SszRound as Encode>::ssz_fixed_len()
    }

    pub fn ssz_bytes_len(round: &Round) -> usize {
        SszRound::ssz_bytes_len(&SszRound::from(*round))
    }

    pub fn ssz_append(round: &Round, buf: &mut Vec<u8>) {
        SszRound::from(*round).ssz_append(buf)
    }
}

pub mod decode {
    use ssz::Decode;

    use super::*;

    pub fn is_ssz_fixed_len() -> bool {
        <SszRound as Decode>::is_ssz_fixed_len()
    }

    pub fn ssz_fixed_len() -> usize {
        <SszRound as Decode>::ssz_fixed_len()
    }

    pub fn from_ssz_bytes(bytes: &[u8]) -> Result<Round, ssz::DecodeError> {
        Ok(SszRound::from_ssz_bytes(bytes)?.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use malachitebft_core_types::Round;
    use ssz::{Decode, Encode};

    #[test]
    fn test_ssz_round_encode_decode() {
        let round = Round::new(1);
        let ssz_round = SszRound::from(round);
        let encoded = ssz_round.as_ssz_bytes();
        let decoded: Round = SszRound::from_ssz_bytes(&encoded).unwrap().into();
        assert_eq!(round, decoded);

        let nil_round = Round::Nil;
        let ssz_nil_round = SszRound::from(nil_round);
        let encoded = ssz_nil_round.as_ssz_bytes();
        let decoded: Round = SszRound::from_ssz_bytes(&encoded).unwrap().into();
        assert_eq!(nil_round, decoded);

        assert!(!<SszRound as ssz::Encode>::is_ssz_fixed_len());
    }
}
