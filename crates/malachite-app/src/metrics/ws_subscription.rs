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

//! RPC-sync `newHeads` WebSocket subscription metrics.
//!
//! Registered against the same `SharedRegistry` as `AppMetrics`/`DbMetrics`/`ProcessMetrics`
//! so the counter is exported by the consensus `/metrics` endpoint with the standard
//! `moniker` label.

use std::sync::OnceLock;

use malachitebft_app_channel::app::metrics::prometheus::metrics::counter::Counter;
use malachitebft_app_channel::app::metrics::SharedRegistry;

static STALE_COUNTER: OnceLock<Counter> = OnceLock::new();

/// Records that one `newHeads` subscription was declared stale and reconnected.
/// No-op if called before [`WsSubscriptionMetrics::install_global`].
pub fn record_stale() {
    if let Some(counter) = STALE_COUNTER.get() {
        counter.inc();
    }
}

/// Metrics for the RPC-sync WebSocket subscription (see `crate::rpc_sync::ws_subscription`).
#[derive(Clone, Debug, Default)]
pub struct WsSubscriptionMetrics {
    /// `newHeads` subscriptions declared stale and reconnected.
    stale: Counter,
}

impl WsSubscriptionMetrics {
    /// Exported as `arc_rpc_sync_ws_stale_total`; `prometheus-client` adds the `_total` suffix.
    pub fn register(registry: &SharedRegistry) -> Self {
        let metrics = Self::default();

        registry.with_prefix("arc_rpc_sync_ws", |registry| {
            registry.register(
                "stale",
                "newHeads subscriptions declared stale (no header within the configured stale timeout) and reconnected",
                metrics.stale.clone(),
            );
        });

        metrics
    }

    /// Install the registered counter as the global used by [`record_stale`].
    /// Idempotent: subsequent calls are ignored.
    pub fn install_global(&self) {
        let _ = STALE_COUNTER.set(self.stale.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_install_and_record_does_not_panic() {
        let registry = SharedRegistry::global().with_moniker("test-ws");
        let metrics = WsSubscriptionMetrics::register(&registry);
        metrics.install_global();

        // Must not panic regardless of whether the process-global OnceLock was
        // already set by another test in the same binary.
        record_stale();
    }
}
