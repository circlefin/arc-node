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

//! Test-wide configuration parameters for Arc integration tests.

use std::time::Duration;

/// Test-wide parameters that apply to all nodes in a test.
///
/// These control consensus timeouts, block production, and other global settings.
/// Individual node settings (like voting power) are configured per-node via
/// [`TestNode`](crate::TestNode).
#[derive(Clone, Debug)]
pub struct TestParams {
    /// Timeout for the propose phase.
    pub consensus_timeout_propose: Duration,
    /// Timeout for the prevote phase.
    pub consensus_timeout_prevote: Duration,
    /// Timeout for the precommit phase.
    pub consensus_timeout_precommit: Duration,
    /// Timeout for the commit phase.
    pub consensus_timeout_commit: Duration,
    /// Target block time (None = no target).
    pub target_block_time: Option<Duration>,
    /// Block gas limit override (None = use chain spec default).
    pub block_gas_limit: Option<u64>,
}

impl Default for TestParams {
    fn default() -> Self {
        Self {
            consensus_timeout_propose: Duration::from_secs(3),
            consensus_timeout_prevote: Duration::from_secs(1),
            consensus_timeout_precommit: Duration::from_secs(1),
            consensus_timeout_commit: Duration::from_millis(500),
            target_block_time: None,
            block_gas_limit: None,
        }
    }
}
