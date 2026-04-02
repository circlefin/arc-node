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

use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::histogram::{exponential_buckets_range, Histogram};
use prometheus_client::registry::Registry;

/// Metrics for remote signing operations
///
/// - `sign_request_count`: Total number of sign requests received
/// - `sign_request_errors`: Total number of sign request errors
/// - `sign_request_latency_total`: Latency of sign requests in seconds (including retries)
/// - `sign_request_latency_single`: Latency of sign requests in seconds (excluding retries)
/// - `sign_request_retries`: Total number of sign request retries
#[derive(Clone, Debug)]
pub struct RemoteSigningMetrics(Arc<Inner>);

impl Deref for RemoteSigningMetrics {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Default for RemoteSigningMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteSigningMetrics {
    pub fn new() -> Self {
        Self(Arc::new(Inner::default()))
    }

    pub fn register(&self, registry: &mut Registry) {
        registry.register(
            "sign_requests_count",
            "Total number of sign requests received",
            self.sign_request_count.clone(),
        );

        registry.register(
            "sign_request_errors",
            "Total number of sign request errors",
            self.sign_request_errors.clone(),
        );

        registry.register(
            "sign_request_retries",
            "Total number of sign request retries",
            self.sign_request_retries.clone(),
        );

        registry.register(
            "sign_request_latency_total",
            "Latency of sign requests in seconds (including retries)",
            self.sign_request_latency_total.clone(),
        );

        registry.register(
            "sign_request_latency_single",
            "Latency of sign requests in seconds (excluding retries)",
            self.sign_request_latency_single.clone(),
        );
    }

    /// Increment the sign requests counter
    pub fn inc_sign_requests(&self) {
        self.sign_request_count.inc();
    }

    /// Increment the sign request errors counter
    pub fn inc_sign_request_errors(&self) {
        self.sign_request_errors.inc();
    }

    /// Increment the sign request retries counter
    pub fn inc_sign_request_retries(&self) {
        self.sign_request_retries.inc();
    }

    /// Observe the latency of a sign request (including retries)
    pub fn observe_sign_request_latency_total(&self, latency: Duration) {
        self.sign_request_latency_total
            .observe(latency.as_secs_f64());
    }

    /// Observe the latency of a sign request (excluding retries)
    pub fn observe_sign_request_latency_single(&self, latency: Duration) {
        self.sign_request_latency_single
            .observe(latency.as_secs_f64());
    }
}

#[derive(Debug)]
pub struct Inner {
    /// Number of sign requests received
    pub sign_request_count: Counter,

    /// Number of sign request errors
    pub sign_request_errors: Counter,

    /// Number of sign request retries
    pub sign_request_retries: Counter,

    /// Latency of sign requests in seconds
    pub sign_request_latency_total: Histogram,

    /// Latency of sign requests in seconds (excluding retries)
    pub sign_request_latency_single: Histogram,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            sign_request_count: Counter::default(),
            sign_request_errors: Counter::default(),
            sign_request_retries: Counter::default(),
            sign_request_latency_total: Histogram::new(exponential_buckets_range(
                0.001, // 1ms
                10.0,  // 10s
                10,
            )),
            sign_request_latency_single: Histogram::new(exponential_buckets_range(
                0.001, // 1ms
                10.0,  // 10s
                10,
            )),
        }
    }
}
