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

use malachitebft_core_types::NilOrVal;

use crate::ValueId;

/// SSZ encoding of NilOrVal.
///
/// Encoded as an option since ValueId could be quite large.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Encode, Decode)]
#[ssz(struct_behaviour = "transparent")]
pub struct SszNilOrVal(Option<ValueId>);

impl From<NilOrVal<ValueId>> for SszNilOrVal {
    fn from(nil_or_val: NilOrVal<ValueId>) -> Self {
        match nil_or_val {
            NilOrVal::Nil => SszNilOrVal(None),
            NilOrVal::Val(value_id) => SszNilOrVal(Some(value_id)),
        }
    }
}

impl From<SszNilOrVal> for NilOrVal<ValueId> {
    fn from(nil_or_val: SszNilOrVal) -> Self {
        match nil_or_val {
            SszNilOrVal(None) => NilOrVal::Nil,
            SszNilOrVal(Some(value_id)) => NilOrVal::Val(value_id),
        }
    }
}

pub mod encode {
    use ssz::Encode;

    use super::*;

    pub fn is_ssz_fixed_len() -> bool {
        <SszNilOrVal as Encode>::is_ssz_fixed_len()
    }

    pub fn ssz_fixed_len() -> usize {
        <SszNilOrVal as Encode>::ssz_fixed_len()
    }

    pub fn ssz_bytes_len(nil_or_val: &NilOrVal<ValueId>) -> usize {
        SszNilOrVal::ssz_bytes_len(&SszNilOrVal::from(*nil_or_val))
    }

    pub fn ssz_append(nil_or_val: &NilOrVal<ValueId>, buf: &mut Vec<u8>) {
        SszNilOrVal::from(*nil_or_val).ssz_append(buf)
    }
}

pub mod decode {
    use ssz::Decode;

    use super::*;

    pub fn is_ssz_fixed_len() -> bool {
        <SszNilOrVal as Decode>::is_ssz_fixed_len()
    }

    pub fn ssz_fixed_len() -> usize {
        <SszNilOrVal as Decode>::ssz_fixed_len()
    }

    pub fn from_ssz_bytes(bytes: &[u8]) -> Result<NilOrVal<ValueId>, ssz::DecodeError> {
        let ssz_nil_or_val = SszNilOrVal::from_ssz_bytes(bytes)?;
        Ok(NilOrVal::from(ssz_nil_or_val))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockHash, ValueId};
    use ssz::{Decode, Encode};

    #[test]
    fn test_ssz_nil_or_val_encode_decode() {
        // nil
        let nil_or_val = NilOrVal::Nil;
        let ssz_nil_or_val = SszNilOrVal::from(nil_or_val);
        let encoded = ssz_nil_or_val.as_ssz_bytes();
        let decoded: NilOrVal<ValueId> = SszNilOrVal::from_ssz_bytes(&encoded).unwrap().into();
        assert_eq!(nil_or_val, decoded);

        // val
        let nil_or_val = NilOrVal::Val(ValueId::new(BlockHash::new([0xAA; 32])));
        let ssz_nil_or_val = SszNilOrVal::from(nil_or_val);
        let encoded = ssz_nil_or_val.as_ssz_bytes();
        let decoded: NilOrVal<ValueId> = SszNilOrVal::from_ssz_bytes(&encoded).unwrap().into();
        assert_eq!(nil_or_val, decoded);

        assert!(!<SszNilOrVal as ssz::Encode>::is_ssz_fixed_len());
    }
}
