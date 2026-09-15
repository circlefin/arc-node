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
    /// Per RFC 9110 §12.5.1, the `Accept` field value is a comma-separated list
    /// of media ranges, each optionally carrying parameters including a `q`
    /// (quality) value. This parses each range individually rather than
    /// matching the whole header as one media type, so headers such as
    /// `application/vnd.arc.v1+json; q=0.9` or
    /// `text/html, application/vnd.arc.v1+json` correctly negotiate V1.
    ///
    /// Among all acceptable ranges (`q` != 0) that map to a supported version,
    /// the one with the highest `q` value is selected (ties keep the
    /// first-encountered range, matching the header's order -- per RFC 9110
    /// §12.5.1 this should really be the *most specific* range on a tie, but
    /// with only one version defined today there is nothing for specificity
    /// to distinguish; this will need revisiting once a second version
    /// exists). A `q` value that fails to parse, or parses outside the valid
    /// `[0, 1]` range -- including `nan`/`inf`/`-inf` (all accepted by
    /// `f32::from_str`'s grammar) and any finite out-of-range value such as
    /// `-1` or `1.5` -- is treated as `q=1`, i.e. *maximally* preferred, not
    /// merely acceptable (RFC 9110 §12.4.1 permits disregarding an
    /// unsatisfiable Accept header entirely, which this is squarely inside
    /// of). With only one version defined today a malformed range can only
    /// ever tie with a conforming one, so this is invisible; once a second
    /// version exists, a malformed q on an old-version range would
    /// out-rank a client's genuinely preferred new-version range -- revisit
    /// this alongside the tie-break-by-specificity gap above when that
    /// happens. Media types and parameter names are matched
    /// case-insensitively per RFC 9110 §8.3.1/§5.6.6/§12.4.2.
    ///
    /// Returns `None` if no range in the header specifies a supported version
    /// (e.g. "text/html" alone, or every versioned range has `q=0`).
    ///
    /// Known deviations from full RFC 9110 conformance, none of which affect
    /// any header this API actually needs to accept: quoted parameter values
    /// containing a comma are not parsed (`;`-params are split naively on
    /// `,`), and `type/*` partial-wildcard ranges are not recognized (only
    /// the literal `*/*`).
    pub fn from_accept_header(value: &str) -> Option<Self> {
        Self::best_match_with_quality(value).map(|(version, _q)| version)
    }

    /// Same as [`Self::from_accept_header`], but also returns the `q` value
    /// of the winning range. Exists mainly so tests can observe the
    /// tie-breaking rule (highest `q` wins) directly, since with only one
    /// [`ApiVersion`] variant defined today, the returned version alone
    /// can't distinguish "highest q" from "first" or "last" range winning.
    fn best_match_with_quality(value: &str) -> Option<(Self, f32)> {
        let trimmed = value.trim();

        // Empty defaults to V1
        if trimmed.is_empty() {
            return Some((Self::default(), 1.0));
        }

        let mut best: Option<(Self, f32)> = None;
        let mut best_q = -1.0f32;

        for media_range in trimmed.split(',') {
            let mut parts = media_range.split(';');
            let media_type = parts.next().unwrap_or("").trim();
            if media_type.is_empty() {
                continue;
            }

            let mut q = 1.0f32;
            for param in parts {
                let param = param.trim();
                if param.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("q=")) {
                    // Per RFC 9110 SS12.4.2, a qvalue is at most 1, with no
                    // negative values, so anything outside [0, 1] -- finite
                    // or not -- cannot come from a conforming sender.
                    // `nan`/`inf`/`-inf` all parse successfully
                    // (f32::from_str's grammar accepts them) rather than
                    // failing to parse, so they must be filtered out
                    // explicitly alongside any finite out-of-range value
                    // (e.g. -1 or 1.5): all of it is equally malformed,
                    // and is treated uniformly as fully acceptable (q=1)
                    // rather than rejecting the range outright.
                    q = param[2..]
                        .trim()
                        .parse::<f32>()
                        .ok()
                        .filter(|v| (0.0..=1.0).contains(v))
                        .unwrap_or(1.0);
                }
            }

            // q=0 explicitly marks this range as not acceptable.
            if q <= 0.0 {
                continue;
            }

            let media_type = media_type.to_ascii_lowercase();
            let version = if media_type == MEDIA_TYPE_JSON || media_type == MEDIA_TYPE_ANY {
                Some(Self::default())
            } else if let Some(version_part) = media_type.strip_prefix(MEDIA_TYPE_PREFIX) {
                version_part
                    .strip_suffix("+json")
                    .and_then(|v| ApiVersion::from_str(v).ok())
            } else {
                None
            };

            if let Some(version) = version {
                if q > best_q {
                    best = Some((version, q));
                    best_q = q;
                }
            }
        }

        best
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
    fn test_from_accept_header_with_quality_parameter() {
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v1+json; q=0.9"),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_multiple_media_ranges() {
        assert_eq!(
            ApiVersion::from_accept_header("text/html, application/vnd.arc.v1+json"),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_generic_json_in_range_list() {
        assert_eq!(
            ApiVersion::from_accept_header("text/html, application/json"),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_wildcard_in_range_list() {
        assert_eq!(
            ApiVersion::from_accept_header("text/plain; q=0.5, */*; q=0.1"),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_zero_quality_is_not_acceptable() {
        // A supported version with q=0 is explicitly marked unacceptable
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v1+json; q=0"),
            None
        );
        // ...even when a generic-JSON range with q=0 is also present
        assert_eq!(
            ApiVersion::from_accept_header("application/json; q=0, application/vnd.arc.v1+json; q=0"),
            None
        );
    }

    #[test]
    fn test_from_accept_header_only_unsupported_versions_in_list() {
        assert_eq!(
            ApiVersion::from_accept_header(
                "application/vnd.arc.v2+json, application/vnd.arc.v99+json"
            ),
            None
        );
    }

    #[test]
    fn test_from_accept_header_supported_range_after_unsupported() {
        assert_eq!(
            ApiVersion::from_accept_header(
                "application/vnd.arc.v99+json, application/vnd.arc.v1+json"
            ),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_whitespace_around_commas_and_parameters() {
        assert_eq!(
            ApiVersion::from_accept_header(
                "  text/html ,  application/vnd.arc.v1+json ; q=0.8  "
            ),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_malformed_quality_value_is_treated_as_acceptable() {
        // An unparsable q value falls back to fully acceptable (q=1) rather
        // than rejecting the range outright.
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v1+json; q=not-a-number"),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_nan_quality_is_treated_as_acceptable() {
        // f32::from_str's grammar accepts "nan" (case-insensitively), so it
        // does not fall into the parse-failure path -- it must be filtered
        // out explicitly (it fails the [0,1] range check), or a NaN q
        // compares false against everything (q<=0.0 is false, so not
        // skipped; q>best_q is also false, so never selected), making the
        // range silently unselectable rather than "fully acceptable" as
        // documented.
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v1+json; q=nan"),
            Some(ApiVersion::V1)
        );
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v1+json; q=NaN"),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_infinite_quality_falls_back_to_acceptable() {
        // f32::from_str also accepts "inf"/"-inf"/"infinity", none of which
        // are producible by a conforming sender (RFC 9110 SS12.4.2 caps a
        // qvalue at 1, with no negative values). Like `nan` and an
        // unparsable string, both fall outside the valid [0,1] range and
        // fall back to the documented q=1 default (see the dedicated
        // out-of-range test below for the finite case, e.g. q=1.5).
        assert_eq!(
            ApiVersion::best_match_with_quality("application/vnd.arc.v1+json; q=inf"),
            Some((ApiVersion::V1, 1.0))
        );
        assert_eq!(
            ApiVersion::best_match_with_quality("application/vnd.arc.v1+json; q=-infinity"),
            Some((ApiVersion::V1, 1.0))
        );
    }

    #[test]
    fn test_from_accept_header_out_of_range_finite_quality_falls_back_to_acceptable() {
        assert_eq!(
            ApiVersion::best_match_with_quality("application/vnd.arc.v1+json; q=-1"),
            Some((ApiVersion::V1, 1.0))
        );
        assert_eq!(
            ApiVersion::best_match_with_quality("application/vnd.arc.v1+json; q=-0.5"),
            Some((ApiVersion::V1, 1.0))
        );
        assert_eq!(
            ApiVersion::best_match_with_quality("application/vnd.arc.v1+json; q=1.5"),
            Some((ApiVersion::V1, 1.0))
        );
    }


    #[test]
    fn test_from_accept_header_media_type_case_insensitive() {
        // RFC 9110 SS8.3.1: "The type and subtype tokens are case-insensitive."
        assert_eq!(
            ApiVersion::from_accept_header("Application/JSON"),
            Some(ApiVersion::V1)
        );
        assert_eq!(
            ApiVersion::from_accept_header("APPLICATION/VND.ARC.V1+JSON"),
            Some(ApiVersion::V1)
        );
    }

    #[test]
    fn test_from_accept_header_quality_parameter_name_case_insensitive() {
        // RFC 9110 SS12.4.2: "a common parameter, named q (case-insensitive)".
        // A capitalized `Q=0` must still mark the range unacceptable -- prior
        // to case-insensitive matching this silently fell back to the q=1
        // default and served a representation the client explicitly
        // rejected.
        assert_eq!(
            ApiVersion::from_accept_header("application/vnd.arc.v1+json; Q=0"),
            None
        );
    }

    #[test]
    fn test_from_accept_header_prefers_highest_quality_supported_range() {
        // Only V1 exists today, so the *version* returned by
        // `from_accept_header` can't distinguish "highest q wins" from
        // "first wins" or "last wins" -- both ranges map to V1 either way.
        // Assert on the winning q via `best_match_with_quality` instead, so
        // this test actually fails if the selection rule regresses (e.g. to
        // first- or last-wins) rather than only failing once a second
        // version exists to tell the outcomes apart.
        assert_eq!(
            ApiVersion::best_match_with_quality(
                "application/vnd.arc.v1+json; q=0.3, application/json; q=0.9"
            ),
            Some((ApiVersion::V1, 0.9))
        );
        // Same ranges, reversed order: still picks q=0.9, confirming this
        // is quality-based selection and not header order.
        assert_eq!(
            ApiVersion::best_match_with_quality(
                "application/json; q=0.9, application/vnd.arc.v1+json; q=0.3"
            ),
            Some((ApiVersion::V1, 0.9))
        );
    }

    #[test]
    fn test_from_str() {
        assert_eq!("v1".parse::<ApiVersion>().unwrap(), ApiVersion::V1);
        assert_eq!("1".parse::<ApiVersion>().unwrap(), ApiVersion::V1);
        assert!("v2".parse::<ApiVersion>().is_err());
        assert!("invalid".parse::<ApiVersion>().is_err());
    }
}
