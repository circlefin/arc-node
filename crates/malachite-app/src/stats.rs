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

use core::fmt;
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use atomic_time::AtomicInstant;
use bytesize::ByteSize;

#[derive(Clone, Default)]
pub struct Stats(Arc<StatsInner>);

impl Deref for Stats {
    type Target = StatsInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct StatsInner {
    /// Total number of transactions processed
    pub txs_count: AtomicU64,
    /// Total number of bytes processed in the chain
    pub chain_bytes: AtomicU64,
    /// Time when the current height started
    pub height_started: AtomicInstant,
    /// Start time of the node
    pub start_time: Instant,
}

impl Default for StatsInner {
    fn default() -> Self {
        Self {
            txs_count: AtomicU64::new(0),
            chain_bytes: AtomicU64::new(0),
            height_started: AtomicInstant::now(),
            start_time: Instant::now(),
        }
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let uptime = self.start_time.elapsed();
        let height_duration = self.height_started().elapsed();
        let txs_rate = self.txs_count() as f64 / uptime.as_secs_f64();
        let bytes_rate = self.chain_bytes().as_u64() as f64 / uptime.as_secs_f64();

        write!(
            f,
            "#txs={}, txs/s={:.2}, chain_bytes={}, bytes/s={:.2}, uptime={:?}, height_duration={:?}",
            self.txs_count(),
            txs_rate,
            self.chain_bytes(),
            bytes_rate,
            uptime,
            height_duration
        )
    }
}

// NOTE: The methods use Relaxed ordering as we don't need strong consistency for these stats.
impl StatsInner {
    pub fn txs_count(&self) -> u64 {
        self.txs_count.load(Ordering::Relaxed)
    }

    pub fn chain_bytes(&self) -> ByteSize {
        ByteSize::b(self.chain_bytes.load(Ordering::Relaxed))
    }

    pub fn add_txs_count(&self, count: u64) {
        self.txs_count.fetch_add(count, Ordering::Relaxed);
    }

    pub fn add_chain_bytes(&self, bytes: u64) {
        self.chain_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn set_height_started(&self, instant: Instant) {
        self.height_started.store(instant, Ordering::Relaxed);
    }

    pub fn height_started(&self) -> Instant {
        self.height_started.load(Ordering::Relaxed)
    }
}
