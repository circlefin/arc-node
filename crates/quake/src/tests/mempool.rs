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

use tracing::debug;

use super::{quake_test, RpcClientFactory, TestOutcome, TestParams, TestResult};
use crate::testnet::Testnet;

/// Assert all node mempools are empty (zero pending, zero queued).
#[quake_test(group = "mempool", name = "empty")]
fn mempool_empty_test<'a>(
    testnet: &'a Testnet,
    _factory: &'a RpcClientFactory,
    _params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        debug!("Checking mempool status on all nodes...");

        let node_urls = testnet.nodes_metadata.all_execution_urls();
        let report = arc_checks::check_mempool(&node_urls).await?;

        let mut outcome = TestOutcome::new();
        for check in report.checks {
            outcome.add_check(check.into());
        }

        outcome
            .auto_summary(
                "All mempools are empty",
                "{} node(s) have leftover transactions",
            )
            .into_result()
    })
}
