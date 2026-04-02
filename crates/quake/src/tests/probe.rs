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

use super::{
    in_parallel, quake_test, CheckResult, RpcClientFactory, TestOutcome, TestParams, TestResult,
};
use crate::testnet::Testnet;

/// Test connectivity to all nodes
#[quake_test(group = "probe", name = "connectivity")]
fn connectivity_test<'a>(
    testnet: &'a Testnet,
    factory: &'a RpcClientFactory,
    _params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        debug!("Probing connectivity...");

        let node_urls = testnet.nodes_metadata.all_execution_urls();
        let results = in_parallel(&node_urls, factory, |client| async move {
            client.get_latest_block_number_with_retries(0).await
        })
        .await;

        let mut outcome = TestOutcome::new();

        for (name, url, result) in results {
            match result {
                Ok(block_number) => {
                    outcome.add_check(CheckResult::success(
                        name,
                        format!("{} (block #{})", url, block_number),
                    ));
                }
                Err(e) => {
                    outcome.add_check(CheckResult::failure(
                        name,
                        format!("{} - Error: {}", url, e),
                    ));
                }
            }
        }

        outcome
            .auto_summary("All nodes are reachable", "{} node(s) are not reachable")
            .into_result()
    })
}

/// Test that all nodes are synced (not syncing)
#[quake_test(group = "probe", name = "sync")]
fn sync_test<'a>(
    testnet: &'a Testnet,
    factory: &'a RpcClientFactory,
    _params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        debug!("Probing sync status...");

        let node_urls = testnet.nodes_metadata.all_execution_urls();
        let results = in_parallel(&node_urls, factory, |client| async move {
            client.is_syncing().await
        })
        .await;

        let mut outcome = TestOutcome::new();

        for (name, url, result) in results {
            match result {
                Ok(is_syncing) => {
                    if is_syncing {
                        outcome.add_check(CheckResult::failure(
                            name,
                            format!("{} - Still syncing", url),
                        ));
                    } else {
                        outcome.add_check(CheckResult::success(
                            name,
                            format!("{} - Synced (not syncing)", url),
                        ));
                    }
                }
                Err(e) => {
                    outcome.add_check(CheckResult::failure(
                        name,
                        format!("{} - Error: {}", url, e),
                    ));
                }
            }
        }

        outcome
            .auto_summary("All nodes are synced", "{} node(s) still syncing")
            .into_result()
    })
}
