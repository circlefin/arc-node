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

use indexmap::IndexMap;

pub(crate) use arc_mesh_analysis::{
    analyze, classify_all, fetch_all_metrics, format_report, parse_all_metrics, MeshDisplayOptions,
    MeshTier, NodeType as MeshNodeType,
};

use crate::manifest::Node;

/// Parse raw Prometheus metrics and assign manifest-aware node types in one step.
///
/// Combines `parse_all_metrics` with `assign_mesh_node_types` so callers don't
/// need to remember both steps.
pub(crate) fn parse_and_classify_metrics(
    raw_metrics: &[(String, String)],
    manifest_nodes: &IndexMap<String, Node>,
) -> Vec<arc_mesh_analysis::NodeMetricsData> {
    let mut nodes_data = parse_all_metrics(raw_metrics);
    if !nodes_data.is_empty() {
        crate::util::assign_mesh_node_types(&mut nodes_data, manifest_nodes);
    }
    nodes_data
}
