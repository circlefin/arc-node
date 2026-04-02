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

use core::fmt;
use std::ops::RangeInclusive;
use std::time::Duration;

use malachitebft_core_types::LinearTimeouts;
use tracing::error;

#[allow(nonstandard_style)]
pub mod bounds {
    use std::ops::RangeInclusive;
    use std::time::Duration;

    pub struct Bounds<T = Duration> {
        pub range: RangeInclusive<T>,
        pub default: T,
    }

    /// Bounds and default for the propose timeout
    pub const timeout_propose: Bounds = Bounds {
        range: Duration::from_millis(500)..=Duration::from_secs(30),
        default: Duration::from_secs(3),
    };

    /// Bounds and default for the prevote timeout
    pub const timeout_prevote: Bounds = Bounds {
        range: Duration::from_millis(250)..=Duration::from_secs(10),
        default: Duration::from_secs(1),
    };

    /// Bounds and default for the precommit timeout
    pub const timeout_precommit: Bounds = Bounds {
        range: Duration::from_millis(250)..=Duration::from_secs(10),
        default: Duration::from_secs(1),
    };

    /// Bounds and default for the rebroadcast timeout
    pub const timeout_rebroadcast: Bounds = Bounds {
        range: Duration::from_secs(1)..=Duration::from_secs(30),
        default: timeout_propose
            .default
            .saturating_add(timeout_prevote.default)
            .saturating_add(timeout_precommit.default),
    };

    /// Bounds and default for the delta values
    pub const timeout_delta: Bounds = Bounds {
        range: Duration::from_millis(50)..=Duration::from_secs(1),
        default: Duration::from_millis(500),
    };

    /// Bounds and default for the target block time
    pub const target_block_time: Bounds = Bounds {
        range: Duration::from_secs(0)..=Duration::from_secs(1),
        default: Duration::from_millis(500),
    };
}

/// Consensus parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsensusParams {
    /// The target block time is used to pause briefly after finalizing a block
    /// before starting the next height, to regulate block production speed.
    ///
    /// If set to None, the block time control mechanism is disabled.
    target_block_time: Option<Duration>,

    /// Timeouts for consensus steps.
    timeouts: LinearTimeouts,
}

impl ConsensusParams {
    pub fn new(target_block_time: Option<Duration>, timeouts: LinearTimeouts) -> Self {
        let mut this = Self {
            target_block_time,
            timeouts,
        };

        this.enforce_bounds();
        this
    }

    pub fn timeouts(&self) -> LinearTimeouts {
        self.timeouts
    }

    pub fn target_block_time(&self) -> Option<Duration> {
        self.target_block_time
    }

    fn enforce_bounds(&mut self) {
        fn adjust(name: &str, value: &mut Duration, bounds: &bounds::Bounds) {
            if !bounds.range.contains(value) {
                error!(
                    "ConsensusParams: {name} value {value:?} out of bounds {}, resetting to default {:?}",
                    Pretty(&bounds.range),
                    bounds.default,
                );

                *value = bounds.default;
            }
        }

        if let Some(target_block_time) = &mut self.target_block_time {
            adjust(
                "target_block_time",
                target_block_time,
                &bounds::target_block_time,
            );
        }

        adjust(
            "timeout_propose",
            &mut self.timeouts.propose,
            &bounds::timeout_propose,
        );
        adjust(
            "timeout_prevote",
            &mut self.timeouts.prevote,
            &bounds::timeout_prevote,
        );
        adjust(
            "timeout_precommit",
            &mut self.timeouts.precommit,
            &bounds::timeout_precommit,
        );
        adjust(
            "timeout_rebroadcast",
            &mut self.timeouts.rebroadcast,
            &bounds::timeout_rebroadcast,
        );

        adjust(
            "propose_delta",
            &mut self.timeouts.propose_delta,
            &bounds::timeout_delta,
        );
        adjust(
            "prevote_delta",
            &mut self.timeouts.prevote_delta,
            &bounds::timeout_delta,
        );
        adjust(
            "precommit_delta",
            &mut self.timeouts.precommit_delta,
            &bounds::timeout_delta,
        );
    }
}

impl Default for ConsensusParams {
    fn default() -> Self {
        Self {
            target_block_time: Some(bounds::target_block_time.default),
            timeouts: LinearTimeouts {
                propose: bounds::timeout_propose.default,
                propose_delta: bounds::timeout_delta.default,
                prevote: bounds::timeout_prevote.default,
                prevote_delta: bounds::timeout_delta.default,
                precommit: bounds::timeout_precommit.default,
                precommit_delta: bounds::timeout_delta.default,
                rebroadcast: bounds::timeout_rebroadcast.default,
            },
        }
    }
}

struct Pretty<'a, T>(pub &'a T);

impl fmt::Display for Pretty<'_, RangeInclusive<Duration>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}..={:?}", self.0.start(), self.0.end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linear_timeouts_default() {
        let params = ConsensusParams::default();
        let linear = params.timeouts();

        // Verify that linear_timeouts correctly maps all timeout fields
        assert_eq!(linear.propose, params.timeouts.propose);
        assert_eq!(linear.propose_delta, params.timeouts.propose_delta);
        assert_eq!(linear.prevote, params.timeouts.prevote);
        assert_eq!(linear.prevote_delta, params.timeouts.prevote_delta);
        assert_eq!(linear.precommit, params.timeouts.precommit);
        assert_eq!(linear.precommit_delta, params.timeouts.precommit_delta);
        assert_eq!(linear.rebroadcast, params.timeouts.rebroadcast);
    }

    #[test]
    fn test_linear_timeouts_custom_values() {
        // All values within valid bounds
        let params = ConsensusParams::new(
            Some(Duration::from_secs(1)),
            LinearTimeouts {
                propose: Duration::from_secs(4),             // within 1s..=5s
                propose_delta: Duration::from_secs(1),       // within 250ms..=1s
                prevote: Duration::from_secs(5),             // within 1s..=5s
                prevote_delta: Duration::from_millis(500),   // within 250ms..=1s
                precommit: Duration::from_secs(3),           // within 1s..=5s
                precommit_delta: Duration::from_millis(300), // within 250ms..=1s
                rebroadcast: Duration::from_secs(5),         // within 3s..=30s
            },
        );

        let linear_timeouts = params.timeouts();

        assert_eq!(linear_timeouts.propose, Duration::from_secs(4));
        assert_eq!(linear_timeouts.propose_delta, Duration::from_secs(1));
        assert_eq!(linear_timeouts.prevote, Duration::from_secs(5));
        assert_eq!(linear_timeouts.prevote_delta, Duration::from_millis(500));
        assert_eq!(linear_timeouts.precommit, Duration::from_secs(3));
        assert_eq!(linear_timeouts.precommit_delta, Duration::from_millis(300));
        assert_eq!(linear_timeouts.rebroadcast, Duration::from_secs(5));
    }

    #[test]
    fn test_bounds_enforcement_timeouts_below_minimum() {
        // All timeouts below minimum (1s) should be reset to defaults
        let params = ConsensusParams::new(
            Some(Duration::from_secs(1)),
            LinearTimeouts {
                propose: Duration::from_millis(250),      // below 500ms min
                propose_delta: Duration::from_millis(20), // below 50ms min
                prevote: Duration::from_millis(100),      // below 1s min
                prevote_delta: Duration::from_millis(30), // below 50ms min
                precommit: Duration::ZERO,                // below 250ms min
                precommit_delta: Duration::ZERO,          // below 50ms min
                rebroadcast: Duration::from_millis(500),  // below 1s min
            },
        );

        let timeouts = params.timeouts();

        // Timeouts should be reset to defaults (3s, 1s, 1s)
        assert_eq!(timeouts.propose, bounds::timeout_propose.default);
        assert_eq!(timeouts.prevote, bounds::timeout_prevote.default);
        assert_eq!(timeouts.precommit, bounds::timeout_precommit.default);

        // Deltas should be reset to default (500ms)
        assert_eq!(timeouts.propose_delta, bounds::timeout_delta.default);
        assert_eq!(timeouts.prevote_delta, bounds::timeout_delta.default);
        assert_eq!(timeouts.precommit_delta, bounds::timeout_delta.default);

        // Rebroadcast is below min (1s), should be reset to default
        assert_eq!(timeouts.rebroadcast, bounds::timeout_rebroadcast.default);
    }

    #[test]
    fn test_bounds_enforcement_timeouts_above_maximum() {
        // All timeouts above maximum should be reset to defaults
        let params = ConsensusParams::new(
            Some(Duration::from_secs(1)),
            LinearTimeouts {
                propose: Duration::from_secs(45),         // above 30s max
                propose_delta: Duration::from_secs(5),    // above 1s max
                prevote: Duration::from_secs(60),         // above 10s max
                prevote_delta: Duration::from_secs(2),    // above 1s max
                precommit: Duration::from_secs(100),      // above 10s max
                precommit_delta: Duration::from_secs(10), // above 1s max
                rebroadcast: Duration::from_secs(100),    // above 30s max
            },
        );

        let timeouts = params.timeouts();

        // Timeouts should be reset to defaults
        assert_eq!(timeouts.propose, bounds::timeout_propose.default);
        assert_eq!(timeouts.prevote, bounds::timeout_prevote.default);
        assert_eq!(timeouts.precommit, bounds::timeout_precommit.default);

        // Deltas should be reset to default
        assert_eq!(timeouts.propose_delta, bounds::timeout_delta.default);
        assert_eq!(timeouts.prevote_delta, bounds::timeout_delta.default);
        assert_eq!(timeouts.precommit_delta, bounds::timeout_delta.default);

        // Rebroadcast is above max (30s), should be reset to default
        assert_eq!(timeouts.rebroadcast, bounds::timeout_rebroadcast.default);
    }

    #[test]
    fn test_bounds_enforcement_target_block_time_out_of_bounds() {
        // Target block time above maximum (1s) should be reset to default (500ms)
        let params = ConsensusParams::new(
            Some(Duration::from_secs(5)), // above 1s max
            LinearTimeouts {
                propose: Duration::from_secs(3),
                propose_delta: Duration::from_millis(500),
                prevote: Duration::from_secs(1),
                prevote_delta: Duration::from_millis(500),
                precommit: Duration::from_secs(1),
                precommit_delta: Duration::from_millis(500),
                rebroadcast: Duration::from_secs(5),
            },
        );

        assert_eq!(
            params.target_block_time(),
            Some(bounds::target_block_time.default)
        );
    }

    #[test]
    fn test_bounds_enforcement_target_block_time_at_zero() {
        // Target block time at 0s is within bounds (0s..=1s)
        let params = ConsensusParams::new(
            Some(Duration::ZERO),
            LinearTimeouts {
                propose: Duration::from_secs(3),
                propose_delta: Duration::from_millis(500),
                prevote: Duration::from_secs(1),
                prevote_delta: Duration::from_millis(500),
                precommit: Duration::from_secs(1),
                precommit_delta: Duration::from_millis(500),
                rebroadcast: Duration::from_secs(5),
            },
        );

        assert_eq!(params.target_block_time(), Some(Duration::ZERO));
    }

    #[test]
    fn test_bounds_enforcement_target_block_time_none() {
        // None target block time should remain None (no bounds check)
        let params = ConsensusParams::new(
            None,
            LinearTimeouts {
                propose: Duration::from_secs(3),
                propose_delta: Duration::from_millis(500),
                prevote: Duration::from_secs(1),
                prevote_delta: Duration::from_millis(500),
                precommit: Duration::from_secs(1),
                precommit_delta: Duration::from_millis(500),
                rebroadcast: Duration::from_secs(5),
            },
        );

        assert_eq!(params.target_block_time(), None);
    }

    #[test]
    fn test_bounds_enforcement_values_at_boundaries() {
        // Values exactly at boundaries should be accepted
        let params = ConsensusParams::new(
            Some(Duration::from_secs(1)), // at max boundary
            LinearTimeouts {
                propose: Duration::from_secs(1),             // at min boundary
                propose_delta: Duration::from_millis(250),   // at min boundary
                prevote: Duration::from_secs(5),             // at max boundary
                prevote_delta: Duration::from_secs(1),       // at max boundary
                precommit: Duration::from_secs(3),           // within bounds
                precommit_delta: Duration::from_millis(500), // within bounds
                rebroadcast: Duration::from_secs(5),
            },
        );

        let timeouts = params.timeouts();

        assert_eq!(params.target_block_time(), Some(Duration::from_secs(1)));
        assert_eq!(timeouts.propose, Duration::from_secs(1));
        assert_eq!(timeouts.propose_delta, Duration::from_millis(250));
        assert_eq!(timeouts.prevote, Duration::from_secs(5));
        assert_eq!(timeouts.prevote_delta, Duration::from_secs(1));
        assert_eq!(timeouts.precommit, Duration::from_secs(3));
        assert_eq!(timeouts.precommit_delta, Duration::from_millis(500));
    }

    #[test]
    fn test_bounds_enforcement_mixed_valid_invalid() {
        // Mix of valid and invalid values - only invalid should be reset
        let params = ConsensusParams::new(
            Some(Duration::from_secs(1)), // valid
            LinearTimeouts {
                propose: Duration::from_secs(2),           // valid
                propose_delta: Duration::ZERO,             // invalid - below min
                prevote: Duration::from_secs(100),         // invalid - above max
                prevote_delta: Duration::from_millis(750), // valid
                precommit: Duration::from_secs(4),         // valid
                precommit_delta: Duration::from_secs(5),   // invalid - above max
                rebroadcast: Duration::from_secs(10),      // valid
            },
        );

        let timeouts = params.timeouts();

        // Valid values should be preserved
        assert_eq!(params.target_block_time(), Some(Duration::from_secs(1)));
        assert_eq!(timeouts.propose, Duration::from_secs(2));
        assert_eq!(timeouts.prevote_delta, Duration::from_millis(750));
        assert_eq!(timeouts.precommit, Duration::from_secs(4));

        // Invalid values should be reset to defaults
        assert_eq!(timeouts.propose_delta, bounds::timeout_delta.default);
        assert_eq!(timeouts.prevote, bounds::timeout_prevote.default);
        assert_eq!(timeouts.precommit_delta, bounds::timeout_delta.default);
    }
}
