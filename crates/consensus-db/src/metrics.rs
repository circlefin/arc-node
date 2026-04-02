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

use malachitebft_app_channel::app::metrics;

use metrics::prometheus::metrics::counter::Counter;
use metrics::prometheus::metrics::gauge::Gauge;
use metrics::prometheus::metrics::histogram::{exponential_buckets, Histogram};
use metrics::SharedRegistry;

/// Metrics for the database.
#[derive(Clone, Debug)]
pub struct DbMetrics(Arc<Inner>);

impl Deref for DbMetrics {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Inner struct for database metrics.
#[derive(Debug)]
pub struct Inner {
    /// Size of the database database (bytes)
    size: Gauge,

    /// Amount of data written to the database (bytes)
    write_bytes: Counter,

    /// Amount of data read from the database (bytes)
    read_bytes: Counter,

    /// Amount of key data read from the database (bytes)
    key_read_bytes: Counter,

    /// Total number of reads from the database
    read_count: Counter,

    /// Total number of writes to the database
    write_count: Counter,

    /// Total number of deletions to the database
    delete_count: Counter,

    /// Time taken to read from the database (seconds)
    read_time: Histogram,

    /// Time taken to write to the database (seconds)
    write_time: Histogram,

    /// Time taken to delete from the database (seconds)
    delete_time: Histogram,
}

impl Inner {
    /// Create a new `Inner` struct.
    pub fn new() -> Self {
        Self {
            size: Gauge::default(),
            write_bytes: Counter::default(),
            read_bytes: Counter::default(),
            key_read_bytes: Counter::default(),
            read_count: Counter::default(),
            write_count: Counter::default(),
            delete_count: Counter::default(),
            read_time: Histogram::new(exponential_buckets(0.001, 2.0, 10)), // Start from 1ms
            write_time: Histogram::new(exponential_buckets(0.001, 2.0, 10)),
            delete_time: Histogram::new(exponential_buckets(0.001, 2.0, 10)),
        }
    }
}

impl Default for Inner {
    fn default() -> Self {
        Self::new()
    }
}

impl DbMetrics {
    /// Create a new `DbMetrics` struct.
    pub fn new() -> Self {
        Self(Arc::new(Inner::new()))
    }

    /// Register the metrics with the given registry.
    pub fn register(registry: &SharedRegistry) -> Self {
        let metrics = Self::new();

        registry.with_prefix("arc_malachite_app_db", |registry| {
            registry.register("size", "Size of the database (bytes)", metrics.size.clone());

            registry.register(
                "write_bytes",
                "Amount of data written to the database (bytes)",
                metrics.write_bytes.clone(),
            );

            registry.register(
                "read_bytes",
                "Amount of data read from the database (bytes)",
                metrics.read_bytes.clone(),
            );

            registry.register(
                "key_read_bytes",
                "Amount of key data read from the database (bytes)",
                metrics.key_read_bytes.clone(),
            );

            registry.register(
                "read_count",
                "Total number of reads from the database",
                metrics.read_count.clone(),
            );

            registry.register(
                "write_count",
                "Total number of writes to the database",
                metrics.write_count.clone(),
            );

            registry.register(
                "delete_count",
                "Total number of deletions to the database",
                metrics.delete_count.clone(),
            );

            registry.register(
                "read_time",
                "Time taken to read bytes from the database (seconds)",
                metrics.read_time.clone(),
            );

            registry.register(
                "write_time",
                "Time taken to write bytes to the database (seconds)",
                metrics.write_time.clone(),
            );

            registry.register(
                "delete_time",
                "Time taken to delete bytes from the database (seconds)",
                metrics.delete_time.clone(),
            );
        });

        metrics
    }

    /// Set the size of the database, in bytes.
    pub fn set_db_size(&self, size: u64) {
        self.size.set(size as i64);
    }

    /// Add the number of bytes written to the database.
    pub fn add_write_bytes(&self, bytes: u64) {
        self.write_bytes.inc_by(bytes);
        self.write_count.inc();
    }

    /// Add the number of bytes read from the database.
    pub fn add_read_bytes(&self, bytes: u64) {
        self.read_bytes.inc_by(bytes);
        self.read_count.inc();
    }

    /// Add the number of bytes read from the database (key only).
    pub fn add_key_read_bytes(&self, bytes: u64) {
        self.key_read_bytes.inc_by(bytes);
    }

    /// Observe the time taken to read from the database.
    pub fn observe_read_time(&self, duration: Duration) {
        self.read_time.observe(duration.as_secs_f64());
    }

    /// Observe the time taken to write to the database.
    pub fn observe_write_time(&self, duration: Duration) {
        self.write_time.observe(duration.as_secs_f64());
    }

    /// Observe the time taken to delete from the database.
    pub fn observe_delete_time(&self, duration: Duration) {
        self.delete_time.observe(duration.as_secs_f64());
    }
}

impl Default for DbMetrics {
    fn default() -> Self {
        Self::new()
    }
}
