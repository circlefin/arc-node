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

//! Helpers for reading individual Prometheus counter values from raw exposition text.

/// Sum the values of a Prometheus counter across every label permutation.
///
/// Matches lines of the form `name <value>` (no labels) and `name{...} <value>` (labelled).
/// Returns `0` when the metric is absent. Useful for asserting on counter increments after
/// an action when the metric is labelled (e.g. by `moniker`) and several series should sum
/// to a meaningful total.
pub fn parse_counter(raw: &str, name: &str) -> u64 {
    let prefix_no_labels = format!("{name} ");
    let prefix_with_labels = format!("{name}{{");
    raw.lines()
        .filter(|line| !line.starts_with('#'))
        .filter(|line| line.starts_with(&prefix_no_labels) || line.starts_with(&prefix_with_labels))
        .filter_map(|line| {
            line.rsplit_once(' ')
                .and_then(|(_, value)| value.parse::<f64>().ok())
        })
        .map(|v| v as u64)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_metric_returns_zero() {
        assert_eq!(parse_counter("", "foo"), 0);
        assert_eq!(
            parse_counter("# HELP foo total\n# TYPE foo counter\n", "foo"),
            0
        );
    }

    #[test]
    fn unlabelled_counter_parses() {
        assert_eq!(parse_counter("foo 42\n", "foo"), 42);
    }

    #[test]
    fn labelled_counter_sums_series() {
        let raw = "foo{moniker=\"a\"} 5\nfoo{moniker=\"b\"} 7\n";
        assert_eq!(parse_counter(raw, "foo"), 12);
    }

    #[test]
    fn ignores_metrics_with_shared_prefix() {
        let raw = "foo 1\nfoo_total 5\nfoobar 9\n";
        assert_eq!(parse_counter(raw, "foo"), 1);
    }

    #[test]
    fn ignores_comment_lines() {
        // A `#` comment that *contains* the metric name must not be parsed.
        let raw = "# foo 99\nfoo 3\n";
        assert_eq!(parse_counter(raw, "foo"), 3);
    }
}
