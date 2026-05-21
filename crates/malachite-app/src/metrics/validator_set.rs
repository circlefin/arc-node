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

//! Validator-set decoding metrics.
//!
//! Registered against the same `SharedRegistry` as `AppMetrics`/`DbMetrics`/`ProcessMetrics`
//! so the counter is exported by the consensus `/metrics` endpoint with the standard
//! `moniker` label.

use malachitebft_app_channel::app::metrics::prometheus::metrics::counter::Counter;
use malachitebft_app_channel::app::metrics::SharedRegistry;

/// Metrics for the `abi_decode_validator_set` decoder living in `arc-eth-engine`.
#[derive(Clone, Debug, Default)]
pub struct ValidatorSetMetrics {
    /// Active validators skipped because their on-chain public key was malformed.
    skipped: Counter,
}

impl ValidatorSetMetrics {
    /// Register the counter under prefix `arc_validator_set`. The exported metric name is
    /// `arc_validator_set_skipped_total` (the `_total` suffix is added by `prometheus-client`).
    pub fn register(registry: &SharedRegistry) -> Self {
        let metrics = Self::default();

        registry.with_prefix("arc_validator_set", |registry| {
            registry.register(
                "skipped",
                "Active validators skipped during validator-set decoding due to malformed public keys",
                metrics.skipped.clone(),
            );
        });

        metrics
    }

    /// Install this counter as the global recorder used by `arc_shared::metrics::validator_set`.
    /// Idempotent: subsequent calls are dropped.
    pub fn install_global(&self) {
        let counter = self.skipped.clone();
        arc_shared::metrics::validator_set::set_recorder(Box::new(move || {
            counter.inc();
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_install_global_does_not_panic() {
        let registry = SharedRegistry::global().with_moniker("test");
        let metrics = ValidatorSetMetrics::register(&registry);
        metrics.install_global();

        // Recording through the shared API must not panic once a recorder is installed.
        arc_shared::metrics::validator_set::record_skipped_validator();
    }
}
