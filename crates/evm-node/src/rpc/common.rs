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

use jsonrpsee::types::{ErrorCode, ErrorObjectOwned};
use serde::de;
use std::fmt;

pub const ARC_DEFAULT_BASE_URL: &str = "http://127.0.0.1:31000";

/// A u64 that deserializes from either a JSON number or a hex quantity string (`"0x…"`).
/// Ethereum proxies (e.g. eRPC) rewrite numeric params to hex; this accepts both.
#[derive(Debug, Clone, Copy)]
pub struct HexOrDecimalU64(u64);

impl HexOrDecimalU64 {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl From<u64> for HexOrDecimalU64 {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

impl<'de> de::Deserialize<'de> for HexOrDecimalU64 {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(HexOrDecimalU64Visitor)
    }
}

struct HexOrDecimalU64Visitor;

impl<'de> de::Visitor<'de> for HexOrDecimalU64Visitor {
    type Value = HexOrDecimalU64;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a u64 or hex-encoded quantity string")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<HexOrDecimalU64, E> {
        Ok(HexOrDecimalU64(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<HexOrDecimalU64, E> {
        let hex = v
            .strip_prefix("0x")
            .or_else(|| v.strip_prefix("0X"))
            .ok_or_else(|| de::Error::custom("hex string must start with 0x"))?;
        u64::from_str_radix(hex, 16)
            .map(HexOrDecimalU64)
            .map_err(de::Error::custom)
    }
}

pub fn invalid_params(msg: impl Into<String>) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(ErrorCode::InvalidParams.code(), msg.into(), None::<()>)
}

pub mod codes {
    /// Artifact not found (e.g., certificate missing upstream).
    pub const NOT_FOUND: i32 = -32004;
    /// Upstream service unreachable (TCP connect failures).
    pub const UPSTREAM_UNREACHABLE: i32 = -32005;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deser(json: &str) -> Result<HexOrDecimalU64, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn accepts_decimal_number() {
        assert_eq!(deser("123").unwrap().as_u64(), 123);
    }

    #[test]
    fn accepts_hex_string() {
        assert_eq!(deser(r#""0x7b""#).unwrap().as_u64(), 123);
    }

    #[test]
    fn accepts_hex_uppercase_prefix() {
        assert_eq!(deser(r#""0X7B""#).unwrap().as_u64(), 123);
    }

    #[test]
    fn accepts_zero() {
        assert_eq!(deser("0").unwrap().as_u64(), 0);
        assert_eq!(deser(r#""0x0""#).unwrap().as_u64(), 0);
    }

    #[test]
    fn rejects_non_hex_string() {
        assert!(deser(r#""abc""#).is_err());
    }

    #[test]
    fn rejects_empty_hex() {
        assert!(deser(r#""0x""#).is_err());
    }
}
