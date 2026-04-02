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

//! API versioning types and utilities

use std::fmt;
use std::str::FromStr;

/// Vendor-specific media type prefix for Arc Network consensus API
pub const MEDIA_TYPE_PREFIX: &str = "application/vnd.arc.v";

/// Fallback media type (generic JSON)
pub const MEDIA_TYPE_JSON: &str = "application/json";

/// Any media type
pub const MEDIA_TYPE_ANY: &str = "*/*";

/// API version for the consensus layer REST endpoints
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApiVersion {
    /// Version 1 (initial version)
    #[default]
    V1,
}

impl ApiVersion {
    /// Returns the version number as a u32
    pub fn as_number(&self) -> u32 {
        match self {
            Self::V1 => 1,
        }
    }

    /// Returns the full media type for this version
    pub fn media_type(&self) -> String {
        format!("{}{}+json", MEDIA_TYPE_PREFIX, self.as_number())
    }

    /// Parses an Accept header value to extract the API version
    ///
    /// Supported formats:
    /// - `application/vnd.arc.v1+json` -> V1
    /// - `application/json` -> default (V1)
    /// - Missing/empty -> default (V1)
    ///
    /// Returns `None` if the header specifies an unsupported version or the
    /// format is unrecognized/malformed (e.g. "text/html")
    pub fn from_accept_header(value: &str) -> Option<Self> {
        let trimmed = value.trim();

        // Empty or generic JSON defaults to V1
        if trimmed.is_empty() || trimmed == MEDIA_TYPE_JSON || trimmed == MEDIA_TYPE_ANY {
            return Some(Self::default());
        }

        // Parse versioned media type
        if let Some(version_part) = trimmed.strip_prefix(MEDIA_TYPE_PREFIX) {
            if let Some(version_str) = version_part.strip_suffix("+json") {
                return ApiVersion::from_str(version_str).ok();
            }
        }

        // Unrecognized/malformed format defaults to None
        None
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.as_number())
    }
}

impl FromStr for ApiVersion {
    type Err = ParseVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "v1" | "1" => Ok(Self::V1),
            _ => Err(ParseVersionError),
        }
    }
}

/// Error returned when parsing an API version fails
#[derive(Debug, Clone, Copy)]
pub struct ParseVersionError;

impl fmt::Display for ParseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid API version")
    }
}

impl std::error::Error for ParseVersionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_version_default() {
        assert_eq!(ApiVersion::default(), ApiVersion::V1);
        assert_eq!(ApiVersion::V1.as_number(), 1);
    }

    #[test]
    fn test_api_version_display() {
        assert_eq!(format!("{}", ApiVersion::V1), "v1");
    }

    #[test]
    fn test_api_version_media_type() {
        assert_eq!(ApiVersion::V1.media_type(), "application/vnd.arc.v1+json");
    }

    #[test]
    fn test_from_accept_header_v1() {
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v1+json"),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_generic_json() {
        assert_eq!(
            ApiVersion::from_accept_header("application/json"),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_any() {
        assert_eq!(ApiVersion::from_accept_header("*/*"), Some(ApiVersion::V1));
    }

    #[test]
    fn test_from_accept_header_empty() {
        assert_eq!(ApiVersion::from_accept_header(""), Some(ApiVersion::V1));
        assert_eq!(ApiVersion::from_accept_header("  "), Some(ApiVersion::V1));
    }

    #[test]
    fn test_from_accept_header_unsupported() {
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v99+json"),
            None
        );
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v2+json"),
            None
        );
    }

    #[test]
    fn test_from_accept_header_malformed() {
        // Malformed formats default to None
        assert_eq!(ApiVersion::from_accept_header("text/html"), None);
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v1"),
            None
        );
        assert_eq!(ApiVersion::from_accept_header("something/random"), None);
    }

    #[test]
    fn test_from_str() {
        assert_eq!("v1".parse::<ApiVersion>().unwrap(), ApiVersion::V1);
        assert_eq!("1".parse::<ApiVersion>().unwrap(), ApiVersion::V1);
        assert!("v2".parse::<ApiVersion>().is_err());
        assert!("invalid".parse::<ApiVersion>().is_err());
    }
}
