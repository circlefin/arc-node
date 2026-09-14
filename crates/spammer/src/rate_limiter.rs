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

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};

use color_eyre::eyre::{self, Result};
use governor::{Jitter, Quota};

/// Token-bucket rate limiter for transaction sending.
///
/// Spaces sends evenly across each second using `governor` instead of
/// resetting a counter once per second. At 1000 TPS each call to `wait()`
/// sleeps ~1ms, eliminating the burst-then-idle pattern of the old approach.
pub(crate) struct RateLimiter {
    limiter: governor::DefaultDirectRateLimiter,
    jitter: Jitter,
    max_num_txs: u64,
    total_counter: AtomicU64,
}

impl RateLimiter {
    pub fn new(tps: u64, max_num_txs: u64, num_senders: usize) -> Result<Self> {
        let tps_u32 = u32::try_from(tps)
            .map_err(|_| eyre::eyre!("TPS must fit in u32, got {tps}"))?;
        let tps_nz =
            NonZeroU32::new(tps_u32).ok_or_else(|| eyre::eyre!("TPS must be greater than 0"))?;
        let burst = (tps / num_senders.max(1) as u64).max(1);
        let burst_u32 = u32::try_from(burst)
            .map_err(|_| eyre::eyre!("burst must fit in u32, got {burst}"))?;
        let burst_nz = NonZeroU32::new(burst_u32)
            .ok_or_else(|| eyre::eyre!("burst must be greater than 0"))?;
        let quota = Quota::per_second(tps_nz).allow_burst(burst_nz);
        let limiter = governor::RateLimiter::direct(quota);
        // Uniformly random jitter up to half the interval
        let jitter = Jitter::up_to(quota.replenish_interval() / 2);
        Ok(Self {
            limiter,
            jitter,
            max_num_txs,
            total_counter: AtomicU64::new(0),
        })
    }

    /// Wait until the rate limiter permits the next send.
    /// Also checks the total transaction limit, if it is set.
    /// Returns `true` to send or `false` to stop, when the transaction limit is reached.
    pub async fn wait(&self) -> bool {
        self.limiter.until_ready_with_jitter(self.jitter).await;
        if self.max_num_txs == 0 {
            return true;
        }
        let prev = self.total_counter.fetch_add(1, Ordering::Relaxed);
        prev < self.max_num_txs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_tps_above_u32_max() {
        let result = RateLimiter::new(u64::from(u32::MAX) + 1, 0, 1);
        let Err(err) = result else {
            panic!("expected oversized TPS to be rejected");
        };
        assert!(err.to_string().contains("TPS must fit in u32"));
    }
}
