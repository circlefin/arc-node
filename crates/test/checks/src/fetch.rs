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

use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use url::Url;

/// Max concurrent metrics fetches to avoid saturating SSM tunnels on large
/// remote testnets (80+ nodes × ~170 KB each through a single SSH session).
const MAX_CONCURRENT_FETCHES: usize = 10;

/// Fetch raw Prometheus metrics text from multiple endpoints in parallel.
///
/// Returns `(node_name, raw_metrics_text)` pairs. Nodes that fail to respond
/// return an empty string for their metrics text.
///
/// Concurrency is capped at [`MAX_CONCURRENT_FETCHES`] to avoid overwhelming
/// narrow transports like SSM tunnels.
pub async fn fetch_all_metrics(metrics_urls: &[(String, Url)]) -> Vec<(String, String)> {
    let mut sorted_urls: Vec<_> = metrics_urls.to_vec();
    sorted_urls.sort_by(|(a, _), (b, _)| a.cmp(b));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    let futures: Vec<_> = sorted_urls
        .iter()
        .map(|(name, url)| {
            let client = client.clone();
            let name = name.clone();
            let url = url.clone();
            async move {
                let body = match client.get(url.as_str()).send().await {
                    Ok(resp) => resp.text().await.unwrap_or_default(),
                    Err(_) => String::new(),
                };
                (name, body)
            }
        })
        .collect();

    stream::iter(futures)
        .buffer_unordered(MAX_CONCURRENT_FETCHES)
        .collect()
        .await
}
