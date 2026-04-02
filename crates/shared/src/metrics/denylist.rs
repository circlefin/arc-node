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

//! Prometheus metrics for denylist rejections at mempool validation.
//!
//! Counter: `arc_tx_denylist_rejection_total`
//!
//! The denylisted address is not included as a label (avoids cardinality); it can be found in logs.

/// Metric name constant.
const DENYLIST_REJECTION: &str = "arc_tx_denylist_rejection_total";

/// Records a denylist rejection to Prometheus.
/// The denylisted address is not included in the metric; it can be found in logs.
pub fn record_denylist_rejection() {
    metrics::counter!(DENYLIST_REJECTION).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_denylist_rejection_does_not_panic() {
        // Smoke test: does not panic (metrics may go to noop if no global recorder)
        record_denylist_rejection();
    }
}
