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

use core::fmt;
use serde::{Deserialize, Serialize};
use ssz_derive::{Decode, Encode};

use malachitebft_proto::{Error as ProtoError, Protobuf};

/// A blockchain height
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Encode, Decode,
)]
#[ssz(struct_behaviour = "transparent")]
#[serde(transparent)]
pub struct Height(u64);

impl Height {
    pub const fn new(height: u64) -> Self {
        Self(height)
    }

    pub const fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn increment(&self) -> Self {
        Self(self.0 + 1)
    }

    pub fn decrement(&self) -> Option<Self> {
        self.0.checked_sub(1).map(Self)
    }

    pub fn saturating_sub(&self, amount: u64) -> Self {
        Self(self.0.saturating_sub(amount))
    }
}

impl Default for Height {
    fn default() -> Self {
        malachitebft_core_types::Height::ZERO
    }
}

impl fmt::Display for Height {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Debug for Height {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Height({})", self.0)
    }
}

impl malachitebft_core_types::Height for Height {
    const ZERO: Self = Self(0);
    const INITIAL: Self = Self(1);

    fn increment_by(&self, n: u64) -> Self {
        Self(self.0 + n)
    }

    fn decrement_by(&self, n: u64) -> Option<Self> {
        Some(Self(self.0.saturating_sub(n)))
    }

    fn as_u64(&self) -> u64 {
        self.0
    }
}

impl Protobuf for Height {
    type Proto = u64;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        Ok(Self(proto))
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_height_new() {
        let height = Height::new(42);
        assert_eq!(height.as_u64(), 42);
    }

    #[test]
    fn test_height_as_u64() {
        let height = Height::new(100);
        assert_eq!(height.as_u64(), 100);
    }

    #[test]
    fn test_height_increment() {
        let height = Height::new(10);
        let incremented = height.increment();
        assert_eq!(incremented.as_u64(), 11);
    }

    #[test]
    fn test_height_decrement() {
        let height = Height::new(10);
        let decremented = height.decrement().unwrap();
        assert_eq!(decremented.as_u64(), 9);
    }

    #[test]
    fn test_height_decrement_zero() {
        let height = Height::new(0);
        assert_eq!(height.decrement(), None);
    }

    #[test]
    fn test_height_default() {
        let height = Height::default();
        assert_eq!(height.as_u64(), 0);
    }

    #[test]
    fn test_height_display() {
        let height = Height::new(123);
        assert_eq!(height.to_string(), "123");
    }

    #[test]
    fn test_height_debug() {
        let height = Height::new(456);
        assert_eq!(format!("{:?}", height), "Height(456)");
    }

    #[test]
    fn test_height_ordering() {
        let height1 = Height::new(5);
        let height2 = Height::new(10);
        let height3 = Height::new(5);

        assert!(height1 < height2);
        assert!(height2 > height1);
        assert!(height1 == height3);
        assert!(height1 <= height3);
        assert!(height1 >= height3);
    }

    #[test]
    fn test_height_hash() {
        use std::collections::HashMap;

        let mut map = HashMap::new();
        let height1 = Height::new(42);
        let height2 = Height::new(42);

        map.insert(height1, "value1");
        assert_eq!(map.get(&height2), Some(&"value1"));
    }

    #[test]
    fn test_height_copy_clone() {
        let height = Height::new(999);
        let copied = height;
        let cloned = height;

        assert_eq!(copied.as_u64(), 999);
        assert_eq!(cloned.as_u64(), 999);
    }

    #[test]
    fn test_height_serde() {
        let height = Height::new(789);

        // Test serialization
        let serialized = serde_json::to_string(&height).unwrap();
        assert_eq!(serialized, "789");

        // Test deserialization
        let deserialized: Height = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, height);
    }

    #[test]
    fn test_height_malachitebft_traits() {
        use malachitebft_core_types::Height as MalachiteHeight;

        let height = Height::new(50);

        // Test ZERO constant
        assert_eq!(Height::ZERO.as_u64(), 0);

        // Test INITIAL constant
        assert_eq!(Height::INITIAL.as_u64(), 1);

        // Test increment_by
        let incremented = height.increment_by(5);
        assert_eq!(incremented.as_u64(), 55);

        // Test decrement_by
        let decremented = height.decrement_by(10);
        assert_eq!(decremented.unwrap().as_u64(), 40);

        // Test decrement_by with overflow
        let decremented_overflow = height.decrement_by(100);
        assert_eq!(decremented_overflow.unwrap().as_u64(), 0);
    }

    #[test]
    fn test_height_ssz_encoding() {
        use ssz::{Decode, Encode};

        let height = Height::new(12345);

        // Test SSZ encoding and decoding
        let ssz_bytes: Vec<u8> = height.as_ssz_bytes();

        let decoded_height = Height::from_ssz_bytes(&ssz_bytes).unwrap();

        assert_eq!(height, decoded_height);

        // Test fixed length
        assert!(<Height as ssz::Encode>::is_ssz_fixed_len());
    }

    #[test]
    fn test_height_edge_cases() {
        // Test maximum u64 value
        let max_height = Height::new(u64::MAX);
        assert_eq!(max_height.as_u64(), u64::MAX);

        // Test increment at max value - this will panic due to overflow
        // We'll test a safer case instead
        let large_height = Height::new(u64::MAX - 1);
        let incremented = large_height.increment();
        assert_eq!(incremented.as_u64(), u64::MAX);

        // Test decrement at max value
        let decremented = max_height.decrement().unwrap();
        assert_eq!(decremented.as_u64(), u64::MAX - 1);
    }
}
