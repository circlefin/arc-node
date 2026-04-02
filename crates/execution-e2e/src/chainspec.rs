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

//! Helper module for creating custom chain specs for e2e tests.
//!
//! Re-exports the test utility from arc_execution_config for convenience.

pub use arc_execution_config::chainspec::{
    localdev_with_block_gas_limit, localdev_with_denylisted_addresses, localdev_with_hardforks,
    localdev_with_protocol_config_reverts, localdev_with_storage_override, BlockGasLimitProvider,
    LOCAL_DEV,
};

#[cfg(test)]
mod tests {
    use super::*;
    use arc_execution_config::hardforks::ArcHardfork;
    use reth_chainspec::Hardforks;

    #[test]
    fn test_localdev_with_hardforks_creates_valid_spec() {
        let spec = localdev_with_hardforks(&[(ArcHardfork::Zero3, 0), (ArcHardfork::Zero4, 5)]);

        // Zero3 should be active at block 0
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero3, 0));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero3, 4));

        // Zero4 should not be active before block 5
        assert!(!spec.is_fork_active_at_block(ArcHardfork::Zero4, 4));
        // Zero4 should be active at and after block 5
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero4, 5));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero4, 10));
    }

    #[test]
    fn test_zero4_at_block_3() {
        let spec = localdev_with_hardforks(&[(ArcHardfork::Zero3, 0), (ArcHardfork::Zero4, 3)]);

        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero3, 0));
        assert!(!spec.is_fork_active_at_block(ArcHardfork::Zero4, 2));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero4, 3));
    }

    #[test]
    fn test_zero5_at_block_5() {
        let spec = localdev_with_hardforks(&[
            (ArcHardfork::Zero3, 0),
            (ArcHardfork::Zero4, 0),
            (ArcHardfork::Zero5, 5),
        ]);

        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero3, 0));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero4, 0));
        assert!(!spec.is_fork_active_at_block(ArcHardfork::Zero5, 4));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero5, 5));
    }

    #[test]
    fn test_zero6_at_block_5() {
        let spec = localdev_with_hardforks(&[
            (ArcHardfork::Zero3, 0),
            (ArcHardfork::Zero4, 0),
            (ArcHardfork::Zero5, 0),
            (ArcHardfork::Zero6, 5),
        ]);

        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero3, 0));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero4, 0));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero5, 0));
        assert!(!spec.is_fork_active_at_block(ArcHardfork::Zero6, 4));
        assert!(spec.is_fork_active_at_block(ArcHardfork::Zero6, 5));
    }
}
