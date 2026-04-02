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
use core::fmt;
use serde::{Deserialize, Serialize};
use ssz_derive::{Decode, Encode};

use malachitebft_proto::{Error as ProtoError, Protobuf};

use crate::proto;
use crate::BlockHash;

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Copy, Serialize, Deserialize, Encode, Decode,
)]
#[ssz(struct_behaviour = "transparent")]
pub struct ValueId(BlockHash);

impl ValueId {
    pub const fn new(block_hash: BlockHash) -> Self {
        Self(block_hash)
    }

    pub const fn block_hash(&self) -> BlockHash {
        self.0
    }
}

impl From<BlockHash> for ValueId {
    fn from(block_hash: BlockHash) -> Self {
        Self::new(block_hash)
    }
}

impl fmt::Display for ValueId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

impl Protobuf for ValueId {
    type Proto = proto::ValueId;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        let block_hash = BlockHash::try_from(proto.block_hash.as_ref())
            .map_err(|_| ProtoError::invalid_data::<Self::Proto>("block_hash"))?;

        Ok(Self::new(block_hash))
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(proto::ValueId {
            block_hash: self.0.to_vec().into(),
        })
    }
}

/// The value to decide on, ie, the hash of the block
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Encode, Decode)]
#[ssz(struct_behaviour = "transparent")]
pub struct Value(BlockHash);

impl Value {
    /// Creates a new Value from a block hash.
    pub const fn new(block_hash: BlockHash) -> Self {
        Self(block_hash)
    }

    pub const fn id(&self) -> ValueId {
        ValueId::new(self.0)
    }

    pub fn size_bytes(&self) -> usize {
        self.0.len()
    }
}

impl malachitebft_core_types::Value for Value {
    type Id = ValueId;

    fn id(&self) -> ValueId {
        self.id()
    }
}

impl Protobuf for Value {
    type Proto = proto::Value;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        let block_hash = BlockHash::try_from(proto.block_hash.as_ref())
            .map_err(|_| ProtoError::invalid_data::<Self::Proto>("value"))?;

        Ok(Self::new(block_hash))
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(proto::Value {
            block_hash: Bytes::from(self.0.to_vec()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_id_ssz_encoding() {
        use ssz::{Decode, Encode};

        let value_id = ValueId::new(BlockHash::new([0; 32]));

        // Test SSZ encoding and decoding
        let ssz_bytes: Vec<u8> = value_id.as_ssz_bytes();

        let decoded_value_id = ValueId::from_ssz_bytes(&ssz_bytes).unwrap();

        assert_eq!(value_id, decoded_value_id);

        // Test fixed length
        assert!(<ValueId as ssz::Encode>::is_ssz_fixed_len());
    }
}
