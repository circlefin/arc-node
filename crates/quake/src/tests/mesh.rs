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

//! Mesh health test for gossipsub connectivity validation.
//!
//! # Overview
//!
//! This test fetches Prometheus metrics from all consensus-layer nodes,
//! analyzes the gossipsub mesh topology, and classifies each node into
//! a health tier. It can optionally enforce topology-derived expectations
//! when `--strict` is passed.
//!
//! # Architecture
//!
//! The test is split into two layers:
//!
//! - **Library** (`arc-mesh-analysis`): topology-agnostic classification.
//!   Produces a raw [`MeshTier`] per node based solely on gossipsub data
//!   (partition membership, direct vs. indirect validator paths).
//!
//! - **Quake test** (this module): manifest-aware enforcement.
//!   Uses the quake manifest to categorize each node and decide what
//!   tier is acceptable for that node's role in the network.
//!
//! # Node categories
//!
//! The manifest is used to infer each node's role:
//!
//! | Category                | How detected                                   | `--strict` expectation                            |
//! |-------------------------|------------------------------------------------|---------------------------------------------------|
//! | `circle-validator`      | Validator without `external = true`            | `FullyConnected` (multi-hop ok if externals exist) |
//! | `external-validator`    | Validator with `external = true`               | Not `NotConnected`                                 |
//! | `consensus-participant` | Non-validator, consensus enabled               | Not `NotConnected`                                 |
//! | `excluded`              | `follow = true` or `consensus.enabled = false` | Skipped                                            |
//!
//! The key insight is that external validators are **expected** to be
//! `MultiHop` — they sit behind dedicated sentries and never have direct
//! mesh links to other validators. Similarly, follow-mode and
//! consensus-disabled nodes don't participate in gossipsub at all.
//!
//! # Usage
//!
//! ```text
//! quake test mesh                       # report only, always passes
//! quake test mesh --set strict=true     # enforce topology expectations
//! ```

use color_eyre::eyre::bail;
use std::fmt;
use tracing::debug;

use super::{quake_test, CheckResult, RpcClientFactory, TestOutcome, TestParams, TestResult};
use crate::manifest::{self, Node};
use crate::mesh::{
    analyze, classify_all, fetch_all_metrics, format_report, parse_all_metrics, MeshDisplayOptions,
    MeshTier,
};
use crate::testnet::Testnet;

/// How the test categorizes a node based on the manifest topology.
/// This determines what mesh tier is acceptable for that node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NodeCategory {
    /// Circle-operated validator (direct mesh expected).
    /// Must be `FullyConnected` in strict mode.
    CircleValidator,

    /// Third-party validator (`external = true` in manifest), typically behind a sentry.
    /// Must not be `NotConnected` in strict mode; `MultiHop` is expected.
    ExternalValidator,

    /// Non-validator with consensus enabled (sentry, full node).
    /// Must not be `NotConnected` in strict mode.
    ConsensusParticipant,

    /// Node with `consensus.enabled = false` or `follow = true`.
    /// Skipped entirely — no mesh expectations.
    Excluded,
}

impl fmt::Display for NodeCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NodeCategory::CircleValidator => "circle-validator",
            NodeCategory::ExternalValidator => "external-validator",
            NodeCategory::ConsensusParticipant => "consensus-participant",
            NodeCategory::Excluded => "excluded",
        })
    }
}

/// Categorize a node based on manifest data.
pub(super) fn categorize_node(
    node_name: &str,
    manifest_node: &Node,
    testnet: &Testnet,
) -> NodeCategory {
    // Follow-mode nodes don't participate in gossip
    if manifest_node.follow {
        return NodeCategory::Excluded;
    }

    // Check consensus_enabled from node metadata (already resolved from cl_config)
    let name = node_name.to_string();
    if let Some(metadata) = testnet.nodes_metadata.get(&name) {
        if !metadata.consensus_enabled {
            return NodeCategory::Excluded;
        }
    }

    // Validators
    if manifest_node.node_type == manifest::NodeType::Validator {
        if manifest_node.external {
            return NodeCategory::ExternalValidator;
        } else {
            return NodeCategory::CircleValidator;
        }
    }

    // Non-validator with consensus enabled
    NodeCategory::ConsensusParticipant
}

/// Check whether a node's tier is acceptable for its category.
/// Returns `Ok(description)` if the node passes, or `Err(reason)` if it fails.
///
/// When external validators are present, circle validators are allowed to be
/// `MultiHop` because indirect paths to external validators (behind sentries)
/// are expected and don't indicate a mesh problem.
pub(super) fn check_strict(
    category: NodeCategory,
    tier: MeshTier,
    has_external_validators: bool,
) -> Result<String, String> {
    match category {
        NodeCategory::CircleValidator => match tier {
            MeshTier::FullyConnected => Ok(format!("{tier}")),
            MeshTier::MultiHop if has_external_validators => Ok(format!(
                "{tier} (ok: indirect paths to external validators expected)"
            )),
            _ => Err(format!("expected fully-connected, got {tier}")),
        },
        NodeCategory::ExternalValidator => {
            if tier == MeshTier::NotConnected {
                Err(format!("expected reachable (multi-hop ok), got {tier}"))
            } else {
                Ok(format!("{tier}"))
            }
        }
        NodeCategory::ConsensusParticipant => {
            if tier == MeshTier::NotConnected {
                Err(format!("expected connected, got {tier}"))
            } else {
                Ok(format!("{tier}"))
            }
        }
        NodeCategory::Excluded => Ok(format!("{tier}")),
    }
}

/// Run mesh analysis and optionally enforce strict tier expectations.
///
/// Fetches gossipsub metrics, analyzes topology, classifies and categorizes
/// each node, prints a report, and (when `strict` is true) returns pass/fail
/// `CheckResult`s. Returns an empty vec when `strict` is false.
///
/// `label` prefixes check names (e.g. `"mesh"`, `"mesh-pre"`).
/// `verbose` controls whether the full mesh report is printed; when false only
/// the per-node classification summary is shown.
pub(super) async fn run_mesh_checks(
    testnet: &Testnet,
    strict: bool,
    label: &str,
    verbose: bool,
) -> color_eyre::eyre::Result<Vec<CheckResult>> {
    let metrics_urls = testnet.nodes_metadata.all_consensus_metrics_urls();
    let raw_metrics = fetch_all_metrics(&metrics_urls).await;
    let nodes_data = parse_all_metrics(&raw_metrics);

    if nodes_data.is_empty() {
        bail!("No mesh metrics collected from any node ({label})");
    }

    let analysis = analyze(&nodes_data);

    if verbose {
        let options = MeshDisplayOptions {
            show_counts: true,
            show_mesh: true,
            show_peers: false,
            show_peers_full: false,
        };
        println!();
        print!("{}", format_report(&analysis, &options));
    }

    let classifications = classify_all(&analysis);

    if label.is_empty() {
        println!("\n── Mesh check ───────────────────────────────────────");
    } else {
        println!("\n── Mesh check ({label}) ──────────────────────────────");
    }
    let mut entries: Vec<(&str, NodeCategory, MeshTier)> = classifications
        .iter()
        .map(|(moniker, _lib_node_type, tier)| {
            let category = testnet
                .manifest
                .nodes
                .get(moniker.as_str())
                .map(|n| categorize_node(moniker, n, testnet))
                .unwrap_or(NodeCategory::Excluded);
            (moniker.as_str(), category, *tier)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    for (moniker, category, tier) in &entries {
        let marker = if *category == NodeCategory::Excluded {
            "⊘"
        } else {
            match tier {
                MeshTier::FullyConnected => "✓",
                MeshTier::MultiHop => "~",
                MeshTier::NotConnected => "✗",
            }
        };
        println!("  {marker} {moniker} ({category}): {tier}");
    }

    if !strict {
        return Ok(vec![]);
    }

    println!();
    let has_external_validators = entries
        .iter()
        .any(|(_, cat, _)| *cat == NodeCategory::ExternalValidator);

    let mut checks = Vec::new();
    for (moniker, category, tier) in &entries {
        if *category == NodeCategory::Excluded {
            continue;
        }
        let check_name = if label.is_empty() {
            moniker.to_string()
        } else {
            format!("{label}:{moniker}")
        };
        match check_strict(*category, *tier, has_external_validators) {
            Ok(desc) => checks.push(CheckResult::success(
                check_name,
                format!("{category}: {desc}"),
            )),
            Err(reason) => checks.push(CheckResult::failure(
                check_name,
                format!("{category}: {reason}"),
            )),
        }
    }
    Ok(checks)
}

/// Mesh health test: fetch gossipsub metrics, report connectivity,
/// and optionally enforce topology-derived expectations.
///
/// Without `--strict`, this is report-only and always passes.
/// With `--strict`, the test uses the manifest to categorize each node
/// and enforces tier expectations:
///   - Circle validators: must be fully-connected (multi-hop allowed when external validators exist)
///   - External validators (behind sentries): must not be isolated
///   - Sentries and full nodes (consensus enabled): must not be isolated
///   - Nodes with consensus disabled or follow mode: skipped
#[quake_test(group = "mesh", name = "health")]
fn health_test<'a>(
    testnet: &'a Testnet,
    _factory: &'a RpcClientFactory,
    params: &'a TestParams,
) -> TestResult<'a> {
    Box::pin(async move {
        debug!("Running mesh health test...");

        let strict = params.get("strict").is_some();
        let checks = run_mesh_checks(testnet, strict, "", true).await?;

        if !strict {
            println!("\nMesh health report complete (use --strict to enforce expectations).");
            return Ok(());
        }

        if checks.is_empty() {
            println!("⚠ No nodes with mesh expectations found");
            return Ok(());
        }

        let mut outcome = TestOutcome::new();
        for check in checks {
            outcome.add_check(check);
        }

        outcome
            .auto_summary(
                "All nodes meet mesh health expectations",
                "{} node(s) failed mesh health checks",
            )
            .into_result()
    })
}
