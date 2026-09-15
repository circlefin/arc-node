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

use color_eyre::eyre::{self, Result, WrapErr};
use governor::{Jitter, Quota};

/// Highest rate `governor` can represent without rounding its per-token
/// replenish interval down to zero nanoseconds.
pub(crate) const MAX_TPS: u64 = 1_000_000_000;

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
        if tps > MAX_TPS {
            eyre::bail!("rate {tps} exceeds the maximum supported rate {MAX_TPS}");
        }
        let tps_u32 = u32::try_from(tps)
            .wrap_err_with(|| format!("rate {tps} exceeds the maximum supported rate {MAX_TPS}"))?;
        let tps_nz =
            NonZeroU32::new(tps_u32).ok_or_else(|| eyre::eyre!("rate must be greater than 0"))?;
        let burst = (tps / num_senders.max(1) as u64).max(1);
        let burst_u32 = u32::try_from(burst).wrap_err("rate limiter burst must fit in u32")?;
        let burst_nz = NonZeroU32::new(burst_u32)
            .ok_or_else(|| eyre::eyre!("rate limiter burst must be greater than 0"))?;
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
    fn new_accepts_max_supported_rate() {
        assert!(RateLimiter::new(MAX_TPS, 1, 1).is_ok());
    }

    #[test]
    fn new_rejects_rate_above_supported_max() {
        let err = RateLimiter::new(MAX_TPS + 1, 1, 1)
            .err()
            .expect("oversized rate must be rejected");
        assert!(err
            .to_string()
            .contains("exceeds the maximum supported rate"));
    }
}
