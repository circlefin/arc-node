use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub(super) const TOPICS: [&str; 3] = ["/consensus", "/proposal_parts", "/liveness"];

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
}
