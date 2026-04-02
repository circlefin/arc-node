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
use std::str::FromStr;

use malachitebft_proto::{Error as ProtoError, Protobuf};

use crate::proto;
use crate::signing::PublicKey;

pub use alloy_primitives::Address as AlloyAddress;

/// Error type for address checksum validation.
#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error(transparent)]
pub struct AddressError(alloy_primitives::AddressError);

/// An Ethereum address, 20 bytes in length.
/// See [`AlloyAddress`] for more details.
#[derive(
    Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Encode, Decode,
)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[ssz(struct_behaviour = "transparent")]
#[serde(transparent)]
pub struct Address(AlloyAddress);

impl Address {
    const LENGTH: usize = 20;

    pub const fn new(value: [u8; Self::LENGTH]) -> Self {
        Self(AlloyAddress::new(value))
    }

    pub fn from_public_key(public_key: &PublicKey) -> Self {
        let hash = hash_public_key(public_key);
        let mut address = [0; Self::LENGTH];
        address.copy_from_slice(&hash[..Self::LENGTH]);
        Self(AlloyAddress::new(address))
    }

    pub fn into_inner(self) -> [u8; Self::LENGTH] {
        self.0.into()
    }

    /// Creates a new [`FixedBytes`] where all bytes are set to `byte`.
    #[inline]
    pub const fn repeat_byte(byte: u8) -> Self {
        Self(AlloyAddress::repeat_byte(byte))
    }

    pub fn to_alloy_address(&self) -> alloy_primitives::Address {
        self.0
    }
}

fn hash_public_key(key: &PublicKey) -> [u8; 32] {
    use sha3::{Digest, Keccak256};
    let mut hasher = Keccak256::new();
    hasher.update(key.as_bytes());
    hasher.finalize().into()
}

impl Default for Address {
    fn default() -> Self {
        Address(AlloyAddress::ZERO)
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0.iter() {
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({self})")
    }
}

impl FromStr for Address {
    type Err = AddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let address = AlloyAddress::from_str(s).map_err(|e| AddressError(e.into()))?;
        Ok(Self(address))
    }
}

impl malachitebft_core_types::Address for Address {}

impl Protobuf for Address {
    type Proto = proto::Address;

    fn from_proto(proto: Self::Proto) -> Result<Self, ProtoError> {
        if proto.value.len() != Self::LENGTH {
            return Err(ProtoError::Other(format!(
                "Invalid address length: expected {}, got {}",
                Self::LENGTH,
                proto.value.len()
            )));
        }

        let mut address = [0; Self::LENGTH];
        address.copy_from_slice(&proto.value);
        Ok(Self(AlloyAddress::new(address)))
    }

    fn to_proto(&self) -> Result<Self::Proto, ProtoError> {
        Ok(proto::Address {
            value: self.0.to_vec().into(),
        })
    }
}

impl From<AlloyAddress> for Address {
    fn from(addr: AlloyAddress) -> Self {
        Self::new(addr.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_into_inner() {
        let address_bytes = [0xAA; 20];
        let address = Address::new(address_bytes);
        let inner = address.into_inner();
        assert_eq!(inner, address_bytes);
    }

    #[test]
    fn test_address_repeat_byte() {
        let address = Address::repeat_byte(0xFF);
        let inner = address.into_inner();
        assert_eq!(inner, [0xFF; 20]);
    }

    #[test]
    fn test_address_to_alloy_address() {
        let address_bytes = [0x12; 20];
        let address = Address::new(address_bytes);
        let alloy_address = address.to_alloy_address();
        assert_eq!(
            <alloy_primitives::Address as Into<[u8; 20]>>::into(alloy_address),
            address_bytes
        );
    }

    #[test]
    fn test_address_display() {
        let address_bytes = [
            0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC,
        ];
        let address = Address::new(address_bytes);
        let display_string = address.to_string();
        assert_eq!(display_string, "123456789ABCDEF0112233445566778899AABBCC");
    }

    #[test]
    fn test_address_debug() {
        let address_bytes = [0xAA; 20];
        let address = Address::new(address_bytes);
        let debug_string = format!("{:?}", address);
        assert_eq!(
            debug_string,
            "Address(AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA)"
        );
    }

    #[test]
    fn test_address_ordering() {
        let address1 = Address::new([0x00; 20]);
        let address2 = Address::new([0x01; 20]);
        let address3 = Address::new([0x00; 20]);

        assert!(address1 < address2);
        assert!(address2 > address1);
        assert!(address1 == address3);
        assert!(address1 <= address3);
        assert!(address1 >= address3);
    }

    #[test]
    fn test_address_hash() {
        use std::collections::HashMap;

        let mut map = HashMap::new();
        let address1 = Address::new([0x42; 20]);
        let address2 = Address::new([0x42; 20]);

        map.insert(address1, "value1");
        assert_eq!(map.get(&address2), Some(&"value1"));
    }

    #[test]
    fn test_address_copy() {
        let address = Address::new([0x99; 20]);
        let copied = address; // copy

        assert_eq!(copied.into_inner(), [0x99; 20]);
    }

    #[test]
    fn test_address_serde() {
        let address = Address::new([0x78; 20]);

        // Test serialization
        let serialized = serde_json::to_string(&address).unwrap();
        assert_eq!(serialized, "\"0x7878787878787878787878787878787878787878\"");

        // Test deserialization
        let deserialized: Address = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, address);
    }

    #[test]
    fn test_address_from_alloy_address() {
        let alloy_address = AlloyAddress::new([0x55; 20]);
        let address: Address = alloy_address.into();
        assert_eq!(address.into_inner(), [0x55; 20]);
    }

    #[test]
    fn test_address_ssz_encoding() {
        use ssz::{Decode, Encode};

        let address = Address::new([
            0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
            0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC,
        ]);

        // Test SSZ encoding and decoding
        let ssz_bytes: Vec<u8> = address.as_ssz_bytes();

        let decoded_address = Address::from_ssz_bytes(&ssz_bytes).unwrap();

        assert_eq!(address, decoded_address);

        // Test empty bytes
        let address = Address::new([0x00; 20]);
        let ssz_bytes: Vec<u8> = address.as_ssz_bytes();
        let decoded_address = Address::from_ssz_bytes(&ssz_bytes).unwrap();
        assert_eq!(address, decoded_address);

        // Test fixed length
        assert!(<Address as ssz::Encode>::is_ssz_fixed_len());
    }

    #[test]
    fn test_from_str() {
        // Test with empty string
        let err = Address::from_str("").unwrap_err();
        assert!(err.to_string().contains("invalid string length"));

        // Test with all-whitespaces string
        let err = Address::from_str("       ").unwrap_err();
        assert!(err.to_string().contains("odd number of digits"));

        // Test with valid address
        let address = Address::from_str("0x52908400098527886E0F7030069857D2E4169EE7").unwrap();
        assert_eq!(
            address,
            Address::new([
                0x52, 0x90, 0x84, 0x00, 0x09, 0x85, 0x27, 0x88, 0x6E, 0x0F, 0x70, 0x30, 0x06, 0x98,
                0x57, 0xD2, 0xE4, 0x16, 0x9E, 0xE7
            ])
        );

        // Test with invalid address (wrong length)
        let err = Address::from_str("0x123").unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("invalid string length") || err_msg.contains("odd number of digits"),
            "Expected 'invalid string length' or 'odd number of digits', got: {}",
            err_msg
        );

        // Test with invalid address (non-hex characters)
        let err = Address::from_str("0xGHIJKL00098527886E0F7030069857D2E4169EE7").unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("invalid character"),
            "Expected 'invalid character', got: {}",
            err_msg
        );
    }
}
