// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
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

use chrono::{DateTime, SecondsFormat, Utc};

/// Return current time in milliseconds since Unix epoch.
///
/// The latency pipeline uses wall-clock timestamps so the query tool
/// can filter by time ranges.
pub(crate) fn timestamp_now() -> u64 {
    let now = std::time::SystemTime::now();
    let dur = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    dur.as_millis() as u64
}

/// Convert Unix epoch milliseconds to a human-readable RFC 3339
/// timestamp.
///
/// CSV consumers need a standard, parseable format for time-based
/// queries and visualization. Raw milliseconds would require
/// post-processing.
pub(super) fn format_rfc3339_millis_utc(unix_ms: u64) -> String {
    let ms_i64 = i64::try_from(unix_ms).unwrap_or(i64::MAX);
    let dt = DateTime::<Utc>::from_timestamp_millis(ms_i64)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Convert Unix epoch seconds to RFC 3339 format.
///
/// Block timestamps from the RPC are in seconds, but we want
/// consistent millisecond-precision formatting across all timestamp
/// columns.
pub(super) fn format_rfc3339_secs_utc(unix_s: u64) -> String {
    format_rfc3339_millis_utc(unix_s.saturating_mul(1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_rfc3339_millis_utc_table() {
        let cases: &[(u64, &str, &str)] = &[
            (0, "1970-01-01T00:00:00.000Z", "Unix epoch"),
            (
                1_704_067_200_000,
                "2024-01-01T00:00:00.000Z",
                "2024-01-01 midnight",
            ),
            (
                1_704_067_200_123,
                "2024-01-01T00:00:00.123Z",
                "sub-second precision",
            ),
        ];

        for (unix_ms, expected, desc) in cases {
            assert_eq!(
                format_rfc3339_millis_utc(*unix_ms),
                *expected,
                "case '{}': format_rfc3339_millis_utc({})",
                desc,
                unix_ms
            );
        }
    }

    #[test]
    fn format_rfc3339_secs_utc_table() {
        let cases: &[(u64, &str, &str)] = &[
            (0, "1970-01-01T00:00:00.000Z", "Unix epoch"),
            (
                1_704_067_200,
                "2024-01-01T00:00:00.000Z",
                "2024-01-01 midnight",
            ),
        ];

        for (unix_secs, expected, desc) in cases {
            assert_eq!(
                format_rfc3339_secs_utc(*unix_secs),
                *expected,
                "case '{}': format_rfc3339_secs_utc({})",
                desc,
                unix_secs
            );
        }
    }
}
