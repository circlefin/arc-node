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

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub(super) const TOPICS: [&str; 3] = ["/consensus", "/proposal_parts", "/liveness"];

/// Aggregated message receive counts across all topics (total and after dedup).
#[derive(Debug, Clone, Default)]
pub struct MessageCounts {
    /// Total messages received (including duplicates), summed across all topics.
    pub unfiltered: u64,
    /// Messages after deduplication, summed across all topics.
    pub filtered: u64,
}

impl MessageCounts {
    pub fn duplicates(&self) -> u64 {
        self.unfiltered.saturating_sub(self.filtered)
    }

    /// Duplicate percentage (0.0–100.0). Returns 0.0 when no messages received.
    pub fn duplicate_pct(&self) -> f64 {
        if self.unfiltered == 0 {
            return 0.0;
        }
        (self.duplicates() as f64 / self.unfiltered as f64) * 100.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NodeType {
    FullNode,
    PersistentPeer,
    Validator,
}

impl fmt::Display for NodeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(match self {
            NodeType::FullNode => "full_node",
            NodeType::PersistentPeer => "persistent",
            NodeType::Validator => "validator",
        })
    }
}

#[derive(Debug, Clone)]
pub struct NodeMetricsData {
    pub moniker: String,
    pub node_type: NodeType,

    /// mesh peer counts per topic hash (e.g. "/consensus" -> 3)
    pub mesh_counts: BTreeMap<String, i64>,

    /// mesh peer monikers per topic name (e.g. "/consensus" -> ["val0", "val1"])
    pub mesh_peers: BTreeMap<String, Vec<String>>,

    /// explicit gossipsub peers (monikers)
    pub explicit_peers: Vec<String>,

    /// Per-peer detail from `malachitebft_network_discovered_peers`
    /// (moniker -> discovered peer info as seen by this node)
    pub discovered_peers: BTreeMap<String, DiscoveredPeer>,

    /// Gossipsub message counts (aggregate across all topics) for duplicate analysis.
    pub message_counts: MessageCounts,

    // connection counts
    pub connected_peers: i64,
    pub inbound_peers: i64,
    pub outbound_peers: i64,
    pub active_connections: i64,
    pub inbound_connections: i64,
    pub outbound_connections: i64,
}

/// Detail about a peer as seen by a particular node, extracted from
/// the `malachitebft_network_discovered_peers` metric.
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub peer_moniker: String,
    pub peer_type: String,
    pub score: f64,
}

#[derive(Debug)]
pub struct TopicAnalysis {
    pub topic_name: String,
    pub meshed_count: usize,
    pub isolated_count: usize,
    pub isolated_nodes: Vec<String>,
    pub partitions: Vec<BTreeSet<String>>,
}

#[derive(Debug)]
pub struct ValidatorConnectivity {
    pub topic_name: String,
    pub all_validators: BTreeSet<String>,
    pub actual_partitions: Vec<BTreeSet<String>>,
    pub direct_val_connections: usize,
    pub max_diameter: usize,
    pub partition_diameters: Vec<Option<usize>>,
    pub completely_isolated: Vec<String>,
    pub isolated_with_explicit: Vec<(String, Vec<String>)>,
    pub validators_without_val_peers: Vec<String>,
    pub indirect_paths: Vec<(String, String, Vec<String>, usize)>,
}

#[derive(Debug)]
pub struct MeshAnalysis {
    pub node_count: usize,
    pub validator_count: usize,
    pub persistent_peer_count: usize,
    pub full_node_count: usize,
    pub nodes: Vec<NodeMetricsData>,
    pub topic_analyses: Vec<TopicAnalysis>,
    pub validator_connectivity: Vec<ValidatorConnectivity>,
    pub zero_mesh_warnings: Vec<(String, i64, i64, i64)>,
}

pub struct MeshDisplayOptions {
    pub show_counts: bool,
    pub show_mesh: bool,
    pub show_peers: bool,
    pub show_peers_full: bool,
    pub show_duplicates: bool,
}
