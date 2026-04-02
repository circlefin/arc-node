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

//! MEV / pending-state surface tests.
//!
//! Verifies that nodes correctly restrict RPC methods that expose pending state
//! or mempool data, which are potential MEV attack vectors.
//!
//! The actual check logic lives in [`arc_checks::mev`]; this module wires it
//! into the Quake test framework.

use tracing::{debug, warn};

use super::{quake_test, RpcClientFactory, TestOutcome, TestParams, TestResult};
use crate::testnet::Testnet;

/// Verify that nodes do not expose pending state or mempool data via RPC.
///
/// Uses `--set nodes=<name1>,<name2>` to identify the nodes whose
/// public RPC surface should be audited (skips when unset).
/// Most validator nodes expose MEV-sensitive methods internally but are not
/// publicly reachable; pass only the nodes with an externally accessible port.
#[quake_test(group = "mev", name = "pending_state")]
fn pending_state_test<'a>(
    testnet: &'a Testnet,
    _factory: &'a RpcClientFactory,
    params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        debug!("Testing MEV / pending-state surface...");

        let Some(nodes_param) = params.get("nodes") else {
            warn!("Skipping: --set nodes=<name1>,<name2> not provided");
            return Ok(());
        };

        let node_names: Vec<&str> = nodes_param.split(',').map(str::trim).collect();

        let node_urls: Vec<_> = testnet
            .nodes_metadata
            .all_execution_urls()
            .into_iter()
            .filter(|(name, _)| node_names.contains(&name.as_str()))
            .collect();

        if node_urls.is_empty() {
            println!("No nodes matched -- check --set nodes=<name>");
            return Ok(());
        }

        let addr = params.get_or("addr", arc_checks::mev::DEFAULT_ADDR);

        let report = arc_checks::check_pending_state(&node_urls, &addr).await?;

        let mut outcome = TestOutcome::new();
        for check in report.checks {
            outcome.add_check(check.into());
        }

        outcome
            .auto_summary(
                "All nodes pass MEV / pending-state surface checks",
                "{} check(s) failed -- review pending-state / namespace configuration",
            )
            .into_result()
    })
}
