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

use alloy_provider::ext::TxPoolApi;
use alloy_provider::ProviderBuilder;
use color_eyre::eyre::Result;
use futures::future::join_all;
use std::time::Duration;
use url::Url;

use crate::types::{CheckResult, Report};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Query `txpool_status` on all nodes and assert zero pending
/// and zero queued transactions.
///
/// Accepts a list of `(node_name, rpc_url)` pairs, e.g.:
/// ```text
/// [
///   ("validator1", "http://localhost:8545"),
///   ("validator2", "http://localhost:8546"),
/// ]
/// ```
pub async fn check_mempool(rpc_urls: &[(String, Url)]) -> Result<Report> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;

    let providers: Vec<_> = rpc_urls
        .iter()
        .map(|(name, url)| {
            let provider = ProviderBuilder::new().connect_reqwest(client.clone(), url.clone());
            (name.clone(), provider)
        })
        .collect();
    check_mempool_with_providers(&providers).await
}

/// Check mempool status using pre-built providers.
///
/// Accepts any `TxPoolApi` implementor, enabling tests to pass
/// mocked providers instead of real HTTP connections.
async fn check_mempool_with_providers<P: TxPoolApi>(providers: &[(String, P)]) -> Result<Report> {
    let futures = providers.iter().map(|(name, provider)| {
        let name = name.clone();
        async move {
            let result = provider.txpool_status().await;
            (name, result)
        }
    });

    // TODO: might want to make this parallel using tokio if the number of nodes to
    // check is large.
    let results = join_all(futures).await;

    let checks = results
        .into_iter()
        .map(|(name, result)| match result {
            Ok(status) => {
                if status.pending == 0 && status.queued == 0 {
                    CheckResult {
                        name,
                        passed: true,
                        message: "mempool empty".to_string(),
                    }
                } else {
                    CheckResult {
                        name,
                        passed: false,
                        message: format!("pending={}, queued={}", status.pending, status.queued),
                    }
                }
            }
            Err(e) => CheckResult {
                name,
                passed: false,
                message: format!("RPC error: {e}"),
            },
        })
        .collect();

    Ok(Report { checks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_provider::mock::Asserter;
    use alloy_rpc_types_txpool::TxpoolStatus;

    fn mock_provider(asserter: &Asserter) -> impl TxPoolApi + '_ {
        ProviderBuilder::new().connect_mocked_client(asserter.clone())
    }

    #[tokio::test]
    async fn all_empty() {
        let asserter = Asserter::new();
        asserter.push_success(&TxpoolStatus {
            pending: 0,
            queued: 0,
        });

        let provider = mock_provider(&asserter);
        let providers = vec![("validator0".to_string(), provider)];
        let report = check_mempool_with_providers(&providers).await.unwrap();

        assert!(report.passed());
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].message, "mempool empty");
    }

    #[tokio::test]
    async fn has_leftover_txs() {
        let asserter = Asserter::new();
        asserter.push_success(&TxpoolStatus {
            pending: 5,
            queued: 2,
        });

        let provider = mock_provider(&asserter);
        let providers = vec![("validator0".to_string(), provider)];
        let report = check_mempool_with_providers(&providers).await.unwrap();

        assert!(!report.passed());
        assert_eq!(report.checks[0].message, "pending=5, queued=2");
    }

    #[tokio::test]
    async fn mixed_results() {
        let a1 = Asserter::new();
        a1.push_success(&TxpoolStatus {
            pending: 0,
            queued: 0,
        });

        let a2 = Asserter::new();
        a2.push_success(&TxpoolStatus {
            pending: 3,
            queued: 0,
        });

        let providers = vec![
            ("validator0".to_string(), mock_provider(&a1)),
            ("validator1".to_string(), mock_provider(&a2)),
        ];
        let report = check_mempool_with_providers(&providers).await.unwrap();

        assert!(!report.passed());

        let val0 = report.checks.iter().find(|c| c.name == "validator0");
        let val1 = report.checks.iter().find(|c| c.name == "validator1");
        assert!(val0.unwrap().passed);
        assert!(!val1.unwrap().passed);
    }

    #[tokio::test]
    async fn rpc_error() {
        let asserter = Asserter::new();
        asserter.push_failure_msg("method not found");

        let provider = mock_provider(&asserter);
        let providers = vec![("validator0".to_string(), provider)];
        let report = check_mempool_with_providers(&providers).await.unwrap();

        assert!(!report.passed());
        assert!(report.checks[0].message.contains("RPC error"));
    }
}
