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

//! Mode-agnostic Prometheus metrics download for Quake testnets.
//!
//! Queries the Prometheus `query_range` API directly over HTTP at
//! `prometheus_url` and bundles the JSON responses into a `.tar.gz`.
//! Local testnets expose Prometheus on a host port via Docker; remote
//! testnets forward the same local port to CC via SSM tunnel, so the
//! same code path handles both without infra-specific dispatch.
//!
//! Defaults match what `download-metrics.sh` did on CC, so this function is
//! a drop-in replacement for `RemoteInfra::download_metrics`.

use std::path::Path;
use std::time::Duration;

use color_eyre::eyre::{eyre, Result, WrapErr};
use tracing::{info, warn};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Query Prometheus and bundle each metric's response into a tarball at `dest`.
///
/// `metric_names` empty → fetch every metric name reported by
/// `/api/v1/label/__name__/values` (matches the old shell script).
/// `from` `None` → use Prometheus' `headStats.minTime` (current head block start).
/// `to` `None` → now.
/// `step` `None` → ceil((to - from)/10 000) seconds, bounded ≥1 (keeps the
/// response below Prometheus' 11 000-point limit).
///
/// Per-metric query failures are logged at warn and skipped — the archive is
/// always produced even if some series aren't currently available.
pub(crate) async fn download_to_tarball(
    prometheus_url: &str,
    metric_names: &[&str],
    from: Option<i64>,
    to: Option<i64>,
    step: Option<&str>,
    dest: &Path,
) -> Result<()> {
    // User-supplied names are interpolated into the response filename, so
    // reject anything outside the Prometheus identifier alphabet to prevent
    // path traversal (e.g. `../../etc/passwd`) and PromQL expressions
    // (e.g. `rate(http_requests_total[5m])`) that wouldn't make a valid file.
    for name in metric_names {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        {
            return Err(eyre!(
                "Invalid metric name '{name}': must match [a-zA-Z0-9_:]+"
            ));
        }
    }

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;

    let now = chrono::Utc::now().timestamp();
    let to = to.unwrap_or(now);
    let from = match from {
        Some(f) => f,
        None => match query_head_min_time(&client, prometheus_url).await {
            Ok(t) => t,
            Err(err) => {
                warn!(%err, "Failed to read Prometheus headStats.minTime; falling back to epoch 0 (archive may cover an unexpectedly sparse range)");
                0
            }
        },
    };

    let step_owned;
    let step = match step {
        Some(s) => s,
        None => {
            step_owned = auto_step(from, to);
            &step_owned
        }
    };

    let names_owned: Vec<String>;
    let names: Vec<&str> = if metric_names.is_empty() {
        names_owned = list_all_metrics(&client, prometheus_url)
            .await
            .wrap_err("list metric names")?;
        names_owned.iter().map(String::as_str).collect()
    } else {
        metric_names.to_vec()
    };

    let tmp = tempfile::tempdir().wrap_err("create metrics temp dir")?;
    let url = format!("{prometheus_url}/api/v1/query_range");
    for name in &names {
        let body = match client
            .get(&url)
            .query(&[
                ("query", *name),
                ("start", &from.to_string()),
                ("end", &to.to_string()),
                ("step", step),
            ])
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    warn!("metric query failed: {name}: read body: {e}");
                    continue;
                }
            },
            Err(e) => {
                warn!("metric query failed: {name}: {e}");
                continue;
            }
        };
        std::fs::write(tmp.path().join(format!("{name}.json")), body)
            .wrap_err_with(|| format!("write {name}.json"))?;
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .wrap_err_with(|| format!("create output dir {}", parent.display()))?;
    }
    // Shell out to `tar` to avoid pulling in flate2/gzip crates for a one-time use.
    let status = std::process::Command::new("tar")
        .arg("czf")
        .arg(dest)
        .arg("-C")
        .arg(tmp.path())
        .arg(".")
        .status()
        .wrap_err("run tar")?;
    if !status.success() {
        return Err(eyre!("tar exited with {status}"));
    }
    info!(path = %dest.display(), "Metrics downloaded");
    Ok(())
}

/// `(to - from)/10 000` seconds, ≥ 1 — matches the old shell script formula.
fn auto_step(from: i64, to: i64) -> String {
    let span = (to - from).max(1);
    let s = ((span + 9_999) / 10_000).max(1);
    format!("{s}s")
}

/// Read Prometheus' current head-block start time (seconds since epoch).
async fn query_head_min_time(client: &reqwest::Client, prometheus_url: &str) -> Result<i64> {
    let url = format!("{prometheus_url}/api/v1/status/tsdb");
    let resp: serde_json::Value = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let min_ms = resp["data"]["headStats"]["minTime"]
        .as_i64()
        .ok_or_else(|| eyre!("missing data.headStats.minTime in /status/tsdb response"))?;
    Ok(min_ms / 1_000)
}

/// List every metric name Prometheus currently knows about.
async fn list_all_metrics(client: &reqwest::Client, prometheus_url: &str) -> Result<Vec<String>> {
    let url = format!("{prometheus_url}/api/v1/label/__name__/values");
    let resp: serde_json::Value = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let arr = resp["data"]
        .as_array()
        .ok_or_else(|| eyre!("missing data array in /label/__name__/values response"))?;
    Ok(arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect())
}
