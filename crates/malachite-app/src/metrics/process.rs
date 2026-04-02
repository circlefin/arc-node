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

#[cfg(not(target_env = "msvc"))]
use malachitebft_app_channel::app::metrics::prometheus::metrics::gauge::Gauge;
use malachitebft_app_channel::app::metrics::SharedRegistry;
use std::sync::Arc;

/// Process metrics for monitoring system resources
#[derive(Clone, Debug)]
pub struct ProcessMetrics(Arc<Inner>);

impl std::ops::Deref for ProcessMetrics {
    type Target = Inner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Jemalloc metrics
#[derive(Debug)]
#[cfg(not(target_env = "msvc"))]
struct JemallocMetricsInner {
    /// Jemalloc active memory in bytes
    active: Gauge,

    /// Jemalloc allocated memory in bytes
    allocated: Gauge,

    /// Jemalloc mapped memory in bytes
    mapped: Gauge,

    /// Jemalloc metadata memory in bytes
    metadata: Gauge,

    /// Jemalloc resident memory in bytes
    resident: Gauge,

    /// Jemalloc retained memory in bytes
    retained: Gauge,
}

/// IO and stats metrics
#[derive(Debug)]
#[cfg(target_os = "linux")]
struct IoMetricsInner {
    /// IO read characters
    rchar: Gauge,

    /// IO written characters
    wchar: Gauge,

    /// IO read syscalls
    syscr: Gauge,

    /// IO write syscalls
    syscw: Gauge,

    /// IO read bytes
    read_bytes: Gauge,

    /// IO written bytes
    write_bytes: Gauge,

    /// IO cancelled write bytes
    cancelled_write_bytes: Gauge,

    /// Process CPU time in seconds (user + system)
    process_cpu_seconds_total: Gauge,

    /// Number of open file descriptors
    process_open_fds: Gauge,

    /// Number of OS threads in the process
    process_threads: Gauge,
}

/// Main inner struct for process metrics
#[derive(Debug)]
pub struct Inner {
    #[cfg(not(target_env = "msvc"))]
    jemalloc: JemallocMetricsInner,

    #[cfg(target_os = "linux")]
    io: IoMetricsInner,
}

#[cfg(not(target_env = "msvc"))]
impl JemallocMetricsInner {
    /// Create a new `JemallocMetricsInner` struct
    pub fn new() -> Self {
        Self {
            active: Gauge::default(),
            allocated: Gauge::default(),
            mapped: Gauge::default(),
            metadata: Gauge::default(),
            resident: Gauge::default(),
            retained: Gauge::default(),
        }
    }

    /// Register the metrics with the given registry
    pub fn register(&self, registry: &SharedRegistry) {
        registry.with_prefix("arc_malachite_app", |registry| {
                registry.register(
                    "jemalloc_active",
                    "Total number of bytes in active pages allocated by the application",
                    self.active.clone(),
                );

                registry.register(
                    "jemalloc_allocated",
                    "Total number of bytes allocated by the application",
                    self.allocated.clone(),
                );

                registry.register(
                    "jemalloc_mapped",
                    "Total number of bytes in active extents mapped by the allocator",
                    self.mapped.clone(),
                );

                registry.register(
                    "jemalloc_metadata",
                    "Total number of bytes dedicated to jemalloc metadata",
                    self.metadata.clone(),
                );

                registry.register(
                    "jemalloc_resident",
                    "Total number of bytes in physically resident data pages mapped by the allocator",
                    self.resident.clone(),
                );

                registry.register(
                    "jemalloc_retained",
                    "Total number of bytes in virtual memory mappings that were retained rather than being returned to the operating system",
                    self.retained.clone(),
                );
            });
    }

    /// Update metrics
    pub fn update(&self) {
        use tracing::error;

        if let Err(error) = tikv_jemalloc_ctl::epoch::advance() {
            error!(%error, "Failed to advance jemalloc epoch");
            return;
        }

        if let Ok(value) = tikv_jemalloc_ctl::stats::active::read() {
            self.active.set(value as i64);
        }

        if let Ok(value) = tikv_jemalloc_ctl::stats::allocated::read() {
            self.allocated.set(value as i64);
        }

        if let Ok(value) = tikv_jemalloc_ctl::stats::mapped::read() {
            self.mapped.set(value as i64);
        }

        if let Ok(value) = tikv_jemalloc_ctl::stats::metadata::read() {
            self.metadata.set(value as i64);
        }

        if let Ok(value) = tikv_jemalloc_ctl::stats::resident::read() {
            self.resident.set(value as i64);
        }

        if let Ok(value) = tikv_jemalloc_ctl::stats::retained::read() {
            self.retained.set(value as i64);
        }
    }
}

#[cfg(not(target_env = "msvc"))]
impl Default for JemallocMetricsInner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "linux")]
impl IoMetricsInner {
    /// Create a new `IoMetricsInner` struct
    pub fn new() -> Self {
        Self {
            rchar: Gauge::default(),
            wchar: Gauge::default(),
            syscr: Gauge::default(),
            syscw: Gauge::default(),
            read_bytes: Gauge::default(),
            write_bytes: Gauge::default(),
            cancelled_write_bytes: Gauge::default(),
            process_cpu_seconds_total: Gauge::default(),
            process_open_fds: Gauge::default(),
            process_threads: Gauge::default(),
        }
    }

    /// Register the metrics with the given registry
    pub fn register(&self, registry: &SharedRegistry) {
        registry.with_prefix("arc_malachite_app", |registry| {
            registry.register("io_rchar", "Characters read", self.rchar.clone());

            registry.register("io_wchar", "Characters written", self.wchar.clone());

            registry.register("io_syscr", "Read syscalls", self.syscr.clone());

            registry.register("io_syscw", "Write syscalls", self.syscw.clone());

            registry.register("io_read_bytes", "Bytes read", self.read_bytes.clone());

            registry.register("io_write_bytes", "Bytes written", self.write_bytes.clone());

            registry.register(
                "io_cancelled_write_bytes",
                "Cancelled write bytes",
                self.cancelled_write_bytes.clone(),
            );

            registry.register(
                "process_cpu_seconds_total",
                "Total user and system CPU time spent in seconds",
                self.process_cpu_seconds_total.clone(),
            );

            registry.register(
                "process_open_fds",
                "Number of open file descriptors",
                self.process_open_fds.clone(),
            );

            registry.register(
                "process_threads",
                "Number of OS threads in the process",
                self.process_threads.clone(),
            );
        });
    }

    /// Update metrics
    pub fn update(&self) {
        use tracing::error;

        let Ok(process) = procfs::process::Process::myself() else {
            error!("Failed to get currently running process");
            return;
        };

        let Ok(io) = process.io() else {
            error!("Failed to get IO stats for the currently running process");
            return;
        };

        // Update IO metrics
        self.rchar.set(io.rchar as i64);
        self.wchar.set(io.wchar as i64);
        self.syscr.set(io.syscr as i64);
        self.syscw.set(io.syscw as i64);
        self.read_bytes.set(io.read_bytes as i64);
        self.write_bytes.set(io.write_bytes as i64);
        self.cancelled_write_bytes
            .set(io.cancelled_write_bytes as i64);

        // Update stats metrics
        if let Ok(stat) = process.stat() {
            // CPU time in seconds (user + system)
            let cpu_time = (stat.utime + stat.stime) as f64 / procfs::ticks_per_second() as f64;
            self.process_cpu_seconds_total.set(cpu_time as i64);

            // Number of threads
            self.process_threads.set(stat.num_threads);
        }

        // Get open file descriptors count
        if let Ok(fds) = process.fd() {
            let fd_count = fds.count();
            self.process_open_fds.set(fd_count as i64);
        }
    }
}

#[cfg(target_os = "linux")]
impl Default for IoMetricsInner {
    fn default() -> Self {
        Self::new()
    }
}

impl Inner {
    /// Create a new `Inner` struct
    pub fn new() -> Self {
        Self {
            #[cfg(not(target_env = "msvc"))]
            jemalloc: JemallocMetricsInner::new(),
            #[cfg(target_os = "linux")]
            io: IoMetricsInner::new(),
        }
    }
}

impl Default for Inner {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessMetrics {
    /// Create a new `ProcessMetrics` struct
    pub fn new() -> Self {
        Self(Arc::new(Inner::new()))
    }

    /// Register the metrics with the given registry
    #[allow(unused_variables)]
    pub fn register(registry: &SharedRegistry) -> Self {
        let metrics = Self::new();

        #[cfg(not(target_env = "msvc"))]
        metrics.jemalloc.register(registry);

        #[cfg(target_os = "linux")]
        metrics.io.register(registry);

        metrics
    }

    /// Update all metrics
    pub fn update_all_metrics(&self) {
        #[cfg(not(target_env = "msvc"))]
        self.jemalloc.update();
        #[cfg(target_os = "linux")]
        self.io.update();
    }
}

impl Default for ProcessMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use malachitebft_app_channel::app::metrics::SharedRegistry;

    /// Test that ProcessMetrics can be created using the new() method
    #[test]
    fn test_process_metrics_new() {
        let metrics = ProcessMetrics::new();
        // Test that we can call methods on it without panicking
        metrics.update_all_metrics();
    }

    /// Test that ProcessMetrics can be created using the default() method
    #[test]
    fn test_process_metrics_default() {
        let metrics = ProcessMetrics::default();
        // Test that we can call methods on it without panicking
        metrics.update_all_metrics();
    }

    /// Test that ProcessMetrics can be cloned
    #[test]
    fn test_process_metrics_clone() {
        let metrics1 = ProcessMetrics::new();
        let metrics2 = metrics1.clone();

        // Both should be usable
        metrics1.update_all_metrics();
        metrics2.update_all_metrics();
    }

    /// Test that ProcessMetrics implements Debug
    #[test]
    fn test_process_metrics_debug() {
        let metrics = ProcessMetrics::new();
        let debug_str = format!("{:?}", metrics);
        assert!(debug_str.contains("ProcessMetrics"));
    }

    /// Test that ProcessMetrics can be registered with a registry
    #[test]
    fn test_process_metrics_register() {
        // Use the global registry for testing
        let registry = SharedRegistry::global().with_moniker("test");
        let metrics = ProcessMetrics::register(&registry);

        // Should not panic and return a valid ProcessMetrics instance
        metrics.update_all_metrics();
    }
}
