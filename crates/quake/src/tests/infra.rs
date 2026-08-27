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

//! Infrastructure-level checks (substrate health, not consensus correctness).
//!
//! Tests in this group probe the underlying infrastructure (containers, hosts,
//! network rules) rather than the chain itself. They require shell access into
//! containers and are therefore slower than `probe:*` / `net:*` tests; the
//! group is excluded from bare `quake test` and must be invoked explicitly
//! (e.g. `quake test infra`). Intended use is operator pre-experiment
//! readiness on a running testnet.
//!
//! # Tests
//!
//! - `infra:latency_emulation` — verify `tc netem` rules inside each node's
//!   CL and EL containers match the manifest. When `latency_emulation = true`,
//!   cross-check each peer's expected delay against `AWS_LATENCY_MATRIX`
//!   within ±10% tolerance. When `latency_emulation = false`, assert no
//!   `netem` qdiscs exist (catches stale rules from a prior latency run).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use color_eyre::eyre::{eyre, Result};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::debug;

use super::{quake_test, CheckResult, RpcClientFactory, TestOutcome, TestParams, TestResult};
use crate::infra::exec::ExecBackend;
use crate::latency;
use crate::node::{NodeName, CONSENSUS_SUFFIX, EXECUTION_SUFFIX};
use crate::testnet::Testnet;

/// Delay match tolerance: actual must be within ±10% of expected.
const DELAY_TOLERANCE: f64 = 0.10;

/// Container interface that carries inter-node traffic. The latency setup
/// script enumerates all `^(eth|ens|eno|enp)` interfaces, but every Quake
/// container today exposes its routed subnet on `eth0`; multi-interface
/// bridge nodes are not handled in this v1 check.
const PROBE_INTERFACE: &str = "eth0";

/// Cap on concurrent `tc` probes. Remote probes go through the Control
/// Center's sshd; the default `MaxStartups 10:30:100` starts dropping
/// connections above ~10 in-flight, so we stay comfortably under it.
const PROBE_CONCURRENCY: usize = 8;

const CONTAINER_SUFFIXES: [&str; 2] = [CONSENSUS_SUFFIX, EXECUTION_SUFFIX];

type ContainerName = &'static str;
type ProbeResult = ((NodeName, ContainerName), Result<ProbeOutputs>);
type IpAddressHex = String;
type ExpectedDelays = HashMap<IpAddressHex, u32>;

#[quake_test(group = "infra", name = "latency_emulation")]
fn latency_emulation_test<'a>(
    testnet: &'a Testnet,
    _factory: &'a RpcClientFactory,
    _params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        let enabled = testnet.manifest.latency_emulation;
        debug!(enabled, "Probing latency emulation rules");
        let backend = ExecBackend::from(testnet);
        let nodes = testnet.nodes_metadata.node_names();
        if enabled {
            let results = probe_all(backend, nodes, true).await;
            let expected = latency::build_expected_delays(&testnet.dir, &testnet.nodes_metadata)?;
            assert_tc_rules_match_matrix(results, expected).await
        } else {
            let results = probe_all(backend, nodes, false).await;
            assert_no_tc_rules(results).await
        }
    })
}

async fn assert_tc_rules_match_matrix(
    results: Vec<ProbeResult>,
    expected: HashMap<NodeName, ExpectedDelays>,
) -> Result<()> {
    let mut outcome = TestOutcome::new();
    for ((node, suffix), result) in results {
        let label = format!("{node}_{suffix}");
        let Some(expected) = expected.get(&node) else {
            outcome.add_check(CheckResult::failure(
                label,
                format!("node '{node}' not in region_assignments.json"),
            ));
            continue;
        };
        let compared = result.and_then(|outputs| {
            let filter = outputs
                .filter
                .as_deref()
                .ok_or_else(|| eyre!("missing filter output (internal bug)"))?;
            compare_against_matrix(&outputs.qdisc, filter, expected)
        });
        match compared {
            Ok(summary) => outcome.add_check(CheckResult::success(label, summary)),
            Err(e) => outcome.add_check(CheckResult::failure(label, e.to_string())),
        }
    }

    outcome
        .auto_summary(
            "All containers match the expected latency matrix",
            "{} container(s) failed latency matrix check",
        )
        .into_result()
}

/// Pure: compare already-fetched `tc` output against the expected matrix.
fn compare_against_matrix(qdisc: &str, filter: &str, expected: &ExpectedDelays) -> Result<String> {
    let handle_to_delay = latency::parse_netem_qdiscs(qdisc);
    let ip_to_handle = latency::parse_u32_filters(filter);

    if handle_to_delay.is_empty() {
        if expected.is_empty() {
            return Ok("no inter-region peers; no latency rules expected".to_string());
        } else {
            return Err(eyre!("no netem qdiscs on {PROBE_INTERFACE}"));
        };
    }

    let mut mismatched = Vec::new();
    let mut checked = 0_usize;
    // Verify every expected peer has a filter routing it to a netem qdisc
    // whose delay matches the matrix.
    for (peer_hex, expected_delay) in expected {
        let Some(handle) = ip_to_handle.get(peer_hex) else {
            mismatched.push(format!("no filter for ip {peer_hex}"));
            continue;
        };
        let Some(actual_delay) = handle_to_delay.get(handle) else {
            mismatched.push(format!("filter 1:{handle} has no netem qdisc"));
            continue;
        };
        let drift = (f64::from(*actual_delay) - f64::from(*expected_delay)).abs()
            / f64::from(*expected_delay);
        if drift > DELAY_TOLERANCE {
            mismatched.push(format!(
                "ip {peer_hex}: expected {expected_delay}ms, got {actual_delay}ms ({:.0}% drift)",
                drift * 100.0
            ));
        }
        checked += 1;
    }

    // Fail on stale rules left by a prior latency config: filters routing peers
    // we don't expect to delay, and netem qdiscs no expected filter points at.
    for ip in ip_to_handle.keys() {
        if !expected.contains_key(ip) {
            mismatched.push(format!("unexpected filter for ip {ip}"));
        }
    }
    let expected_handles: HashSet<&str> = expected
        .keys()
        .filter_map(|ip| ip_to_handle.get(ip))
        .map(String::as_str)
        .collect();
    for handle in handle_to_delay.keys() {
        if !expected_handles.contains(handle.as_str()) {
            mismatched.push(format!("unexpected netem qdisc 1:{handle}"));
        }
    }

    if mismatched.is_empty() {
        Ok(format!(
            "{checked} peer rule(s) within \u{00b1}{:.0}% of matrix",
            DELAY_TOLERANCE * 100.0
        ))
    } else {
        Err(eyre!("{}", mismatched.join("; ")))
    }
}

async fn assert_no_tc_rules(results: Vec<ProbeResult>) -> Result<()> {
    let mut outcome = TestOutcome::new();
    for ((node, suffix), result) in results {
        let label = format!("{node}_{suffix}");
        match result {
            Ok(outputs) => {
                let netems = latency::parse_netem_qdiscs(&outputs.qdisc);
                if netems.is_empty() {
                    outcome.add_check(CheckResult::success(label, "no netem qdiscs"));
                } else {
                    outcome.add_check(CheckResult::failure(
                        label,
                        format!("{} stale netem qdisc(s): {:?}", netems.len(), netems),
                    ));
                }
            }
            Err(e) => outcome.add_check(CheckResult::failure(label, e.to_string())),
        }
    }

    outcome
        .auto_summary(
            "No latency rules present (as expected by manifest)",
            "{} container(s) have stale latency rules",
        )
        .into_result()
}

// ── Parallel probe fan-out ─────────────────────────────────────────────

struct ProbeOutputs {
    qdisc: String,
    /// `None` iff `kind = QdiscOnly`.
    filter: Option<String>,
}

/// Execute per-(node, container) `tc` probes in parallel, capped by
/// `PROBE_CONCURRENCY`. Results are returned with deterministic ordering
/// (sorted by node name, then container suffix) so output stays readable.
async fn probe_all(backend: ExecBackend, nodes: Vec<NodeName>, enabled: bool) -> Vec<ProbeResult> {
    let semaphore = Arc::new(Semaphore::new(PROBE_CONCURRENCY));
    let mut set: JoinSet<ProbeResult> = JoinSet::new();

    for node in nodes {
        for container in CONTAINER_SUFFIXES {
            let backend = backend.clone();
            let semaphore = semaphore.clone();
            let node = node.clone();
            set.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .expect("semaphore never closed");
                let node_for_blocking = node.clone();
                let blocking = tokio::task::spawn_blocking(move || {
                    probe_one(&backend, &node_for_blocking, container, enabled)
                });
                let result = match blocking.await {
                    Ok(res) => res,
                    Err(e) => Err(eyre!("probe task panicked: {e}")),
                };
                ((node, container), result)
            });
        }
    }

    let mut out = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(res) => out.push(res),
            Err(e) => panic!("probe join failed: {e}"),
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn probe_one(
    backend: &ExecBackend,
    node: &NodeName,
    container: &str,
    enabled: bool,
) -> Result<ProbeOutputs> {
    let node = node.to_string();

    let args = ["tc", "qdisc", "show", "dev", PROBE_INTERFACE];
    let qdisc = backend.exec_in_container(&node, container, &args)?;

    let filter = if enabled {
        let args = ["tc", "filter", "show", "dev", PROBE_INTERFACE];
        Some(backend.exec_in_container(&node, container, &args)?)
    } else {
        None
    };

    Ok(ProbeOutputs { qdisc, filter })
}
