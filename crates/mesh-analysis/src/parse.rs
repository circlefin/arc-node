use std::collections::{BTreeMap, HashSet};

use prometheus_parse::{Sample, Scrape, Value};

use super::types::{DiscoveredPeer, NodeMetricsData, NodeType, TOPICS};

/// Metric name prefixes we actually use. Lines not matching any of these
/// (and not starting with `#`) are dropped before parsing, which avoids
/// allocating `Sample` objects for the hundreds of metrics we don't need.
const METRIC_PREFIXES: &[&str] = &[
    "malachitebft_network_gossipsub_mesh_peer_counts",
    "malachitebft_network_peer_mesh_membership",
    "malachitebft_network_explicit_peers",
    "malachitebft_network_discovered_peers",
    "malachitebft_core_consensus_connected_peers",
    "malachitebft_discovery_num_inbound_peers",
    "malachitebft_discovery_num_outbound_peers",
    "malachitebft_discovery_num_active_connections",
    "malachitebft_discovery_num_inbound_connections",
    "malachitebft_discovery_num_outbound_connections",
];

/// Parse raw Prometheus text, keeping only the metrics we care about.
fn parse_metrics(raw: &str) -> Vec<Sample> {
    let filtered: String = raw
        .lines()
        .filter(|line| {
            // Keep comment lines (# TYPE, # HELP) — needed for type inference
            if line.starts_with('#') {
                return line
                    .split_whitespace()
                    .nth(2)
                    .is_some_and(|name| METRIC_PREFIXES.iter().any(|p| name.starts_with(p)));
            }
            METRIC_PREFIXES.iter().any(|p| line.starts_with(p))
        })
        .collect::<Vec<_>>()
        .join("\n");

    let lines = filtered.lines().map(|l| Ok(l.to_owned()));
    Scrape::parse(lines)
        .map(|scrape| scrape.samples)
        .unwrap_or_default()
}

/// Extract the `moniker` label from the first sample that has one.
fn extract_moniker(samples: &[Sample]) -> String {
    for s in samples {
        if let Some(m) = s.labels.get("moniker") {
            return m.to_string();
        }
    }
    "unknown".to_string()
}

/// Extract f64 from a gauge/counter/untyped value.
fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Gauge(f) | Value::Counter(f) | Value::Untyped(f) => Some(*f),
        _ => None,
    }
}

/// Find the first sample matching `metric` and return its value as i64.
fn extract_gauge_value(samples: &[Sample], metric: &str) -> i64 {
    samples
        .iter()
        .find(|s| s.metric == metric)
        .and_then(|s| value_as_f64(&s.value))
        .map(|f| f as i64)
        .unwrap_or(0)
}

/// Find the first sample matching `metric` with a specific label value,
/// and return its value as i64.
fn extract_gauge_value_with_label(
    samples: &[Sample],
    metric: &str,
    label_key: &str,
    label_value: &str,
) -> i64 {
    samples
        .iter()
        .find(|s| s.metric == metric && s.labels.get(label_key).is_some_and(|v| v == label_value))
        .and_then(|s| value_as_f64(&s.value))
        .map(|f| f as i64)
        .unwrap_or(0)
}

/// Collect `peer_moniker` labels from `malachitebft_network_peer_mesh_membership`
/// samples where the `topic` label matches and the gauge value is 1.0.
fn extract_peer_monikers(samples: &[Sample], topic: &str) -> Vec<String> {
    let mut peers = HashSet::new();
    for s in samples {
        if s.metric == "malachitebft_network_peer_mesh_membership"
            && s.labels.get("topic").is_some_and(|t| t == topic)
            && value_as_f64(&s.value) == Some(1.0)
        {
            if let Some(m) = s.labels.get("peer_moniker") {
                peers.insert(m.to_string());
            }
        }
    }
    peers.into_iter().collect()
}

/// Collect sorted `peer_moniker` labels from `malachitebft_network_explicit_peers`
/// samples where the gauge value is 1.0.
fn extract_explicit_peers(samples: &[Sample]) -> Vec<String> {
    let mut peers = HashSet::new();
    for s in samples {
        if s.metric == "malachitebft_network_explicit_peers" && value_as_f64(&s.value) == Some(1.0)
        {
            if let Some(m) = s.labels.get("peer_moniker") {
                peers.insert(m.to_string());
            }
        }
    }
    let mut v: Vec<_> = peers.into_iter().collect();
    v.sort();
    v
}

/// Extract per-peer detail from `malachitebft_network_discovered_peers` for this node.
fn extract_discovered_peers(samples: &[Sample]) -> BTreeMap<String, DiscoveredPeer> {
    let mut peers = BTreeMap::new();
    for s in samples {
        if s.metric != "malachitebft_network_discovered_peers" {
            continue;
        }
        let score = value_as_f64(&s.value).unwrap_or(0.0);
        if score < -1_000_000_000.0 {
            continue; // stale entry
        }
        let Some(peer_moniker) = s.labels.get("peer_moniker") else {
            continue;
        };
        let peer_type = s
            .labels
            .get("peer_type")
            .map(|s| s.to_string())
            .unwrap_or_default();
        peers.insert(
            peer_moniker.to_string(),
            DiscoveredPeer {
                peer_moniker: peer_moniker.to_string(),
                peer_type,
                score,
            },
        );
    }
    peers
}

/// Determine the node type by inspecting `malachitebft_network_discovered_peers`
/// across all other nodes' metrics.
fn determine_node_type(
    target_moniker: &str,
    all_parsed: &[(String, Vec<Sample>)], // (moniker, samples)
) -> NodeType {
    let mut found = NodeType::FullNode;

    for (moniker, samples) in all_parsed {
        if moniker == target_moniker {
            continue;
        }
        for s in samples {
            if s.metric != "malachitebft_network_discovered_peers" {
                continue;
            }
            if s.labels
                .get("peer_moniker")
                .is_none_or(|m| m != target_moniker)
            {
                continue;
            }
            let score = value_as_f64(&s.value).unwrap_or(0.0);
            if score < -1_000_000_000.0 {
                continue; // stale entry
            }
            if s.labels.get("peer_type").is_some_and(|t| t == "validator") {
                return NodeType::Validator;
            }
            if s.labels
                .get("peer_type")
                .is_some_and(|t| t == "persistent_peer")
            {
                found = NodeType::PersistentPeer;
            }
        }
    }
    found
}

pub fn parse_all_metrics(raw_metrics: &[(String, String)]) -> Vec<NodeMetricsData> {
    // First pass: parse each node's raw text into (moniker, Vec<Sample>)
    let parsed: Vec<(String, Vec<Sample>)> = raw_metrics
        .iter()
        .filter(|(_, m)| !m.is_empty())
        .map(|(_, m)| {
            let samples = parse_metrics(m);
            let moniker = extract_moniker(&samples);
            (moniker, samples)
        })
        .collect();

    let running_monikers: HashSet<String> = parsed.iter().map(|(m, _)| m.clone()).collect();

    // Second pass: build full data with cross-node type resolution
    parsed
        .iter()
        .map(|(moniker, samples)| {
            let node_type = determine_node_type(moniker, &parsed);

            let mut mesh_counts = BTreeMap::new();
            let mut mesh_peers = BTreeMap::new();
            for &topic in &TOPICS {
                let count = extract_gauge_value_with_label(
                    samples,
                    "malachitebft_network_gossipsub_mesh_peer_counts",
                    "hash",
                    topic,
                );
                mesh_counts.insert(topic.to_string(), count);

                let peers: Vec<String> = extract_peer_monikers(samples, topic)
                    .into_iter()
                    .filter(|p| running_monikers.contains(p))
                    .collect();
                mesh_peers.insert(topic.to_string(), peers);
            }

            let explicit_peers = extract_explicit_peers(samples);
            let discovered_peers = extract_discovered_peers(samples);

            let connected_peers =
                extract_gauge_value(samples, "malachitebft_core_consensus_connected_peers");
            let inbound_peers =
                extract_gauge_value(samples, "malachitebft_discovery_num_inbound_peers");
            let outbound_peers =
                extract_gauge_value(samples, "malachitebft_discovery_num_outbound_peers");
            let active_connections =
                extract_gauge_value(samples, "malachitebft_discovery_num_active_connections");
            let inbound_connections =
                extract_gauge_value(samples, "malachitebft_discovery_num_inbound_connections");
            let outbound_connections =
                extract_gauge_value(samples, "malachitebft_discovery_num_outbound_connections");

            NodeMetricsData {
                moniker: moniker.clone(),
                node_type,
                mesh_counts,
                mesh_peers,
                explicit_peers,
                discovered_peers,
                connected_peers,
                inbound_peers,
                outbound_peers,
                active_connections,
                inbound_connections,
                outbound_connections,
            }
        })
        .collect()
}
