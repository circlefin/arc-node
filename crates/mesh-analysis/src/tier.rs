use std::fmt;
use std::str::FromStr;

use super::types::{MeshAnalysis, NodeType, ValidatorConnectivity};

/// Mesh health tier for a node.
///
/// Tiers are ordered from healthiest to least healthy:
/// - `FullyConnected`: direct mesh peer links to all relevant peers
/// - `MultiHop`: reachable but only via intermediate relayers
/// - `NotConnected`: isolated or in a minority partition
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeshTier {
    FullyConnected,
    MultiHop,
    NotConnected,
}

impl fmt::Display for MeshTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MeshTier::FullyConnected => "fully-connected",
            MeshTier::MultiHop => "multi-hop",
            MeshTier::NotConnected => "not-connected",
        })
    }
}

impl FromStr for MeshTier {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "fully-connected" => Ok(MeshTier::FullyConnected),
            "multi-hop" => Ok(MeshTier::MultiHop),
            "not-connected" => Ok(MeshTier::NotConnected),
            other => Err(format!(
                "unknown tier '{other}'; expected fully-connected, multi-hop, or not-connected"
            )),
        }
    }
}

/// Classify a single node's mesh health on the `/consensus` topic.
fn classify_validator(moniker: &str, consensus_connectivity: &ValidatorConnectivity) -> MeshTier {
    // Completely isolated → not connected
    if consensus_connectivity
        .completely_isolated
        .iter()
        .any(|v| v == moniker)
    {
        return MeshTier::NotConnected;
    }

    // Isolated with only explicit peers (no mesh peers) → not connected
    if consensus_connectivity
        .isolated_with_explicit
        .iter()
        .any(|(v, _)| v == moniker)
    {
        return MeshTier::NotConnected;
    }

    // Multiple partitions: node not in the largest partition → not connected
    if consensus_connectivity.actual_partitions.len() > 1 {
        let max_size = consensus_connectivity
            .actual_partitions
            .iter()
            .map(|p| p.len())
            .max()
            .unwrap_or(0);

        let in_largest = consensus_connectivity
            .actual_partitions
            .iter()
            .filter(|p| p.len() == max_size)
            .any(|p| p.contains(moniker));

        if !in_largest {
            return MeshTier::NotConnected;
        }
    }

    // No direct validator mesh peers but has some mesh peers → multi-hop
    if consensus_connectivity
        .validators_without_val_peers
        .iter()
        .any(|v| v == moniker)
    {
        return MeshTier::MultiHop;
    }

    // Has indirect paths through non-validator nodes → multi-hop
    if consensus_connectivity
        .indirect_paths
        .iter()
        .any(|(v1, v2, _, _)| v1 == moniker || v2 == moniker)
    {
        return MeshTier::MultiHop;
    }

    MeshTier::FullyConnected
}

/// Classify a non-validator node on the `/consensus` topic.
fn classify_non_validator(moniker: &str, analysis: &MeshAnalysis) -> MeshTier {
    let consensus_topic = analysis
        .topic_analyses
        .iter()
        .find(|t| t.topic_name == "/consensus");

    let Some(topic) = consensus_topic else {
        return MeshTier::NotConnected;
    };

    // Isolated on /consensus → not connected
    if topic.isolated_nodes.iter().any(|n| n == moniker) {
        return MeshTier::NotConnected;
    }

    // Single partition → fully connected
    if topic.partitions.len() <= 1 {
        return MeshTier::FullyConnected;
    }

    // Multiple partitions: check if in any partition of maximum size
    let max_size = topic.partitions.iter().map(|p| p.len()).max().unwrap_or(0);

    let in_largest = topic
        .partitions
        .iter()
        .filter(|p| p.len() == max_size)
        .any(|p| p.contains(moniker));

    if in_largest {
        return MeshTier::FullyConnected;
    }

    MeshTier::MultiHop
}

/// Classify every node in the analysis and return a vec of (moniker, node_type, tier).
pub fn classify_all(analysis: &MeshAnalysis) -> Vec<(String, NodeType, MeshTier)> {
    let consensus_connectivity = analysis
        .validator_connectivity
        .iter()
        .find(|vc| vc.topic_name == "/consensus");

    analysis
        .nodes
        .iter()
        .map(|node| {
            let tier = if node.node_type == NodeType::Validator {
                if let Some(vc) = consensus_connectivity {
                    classify_validator(&node.moniker, vc)
                } else {
                    // No validator connectivity data → can't classify
                    MeshTier::NotConnected
                }
            } else {
                classify_non_validator(&node.moniker, analysis)
            };
            (node.moniker.clone(), node.node_type, tier)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ValidatorConnectivity;
    use std::collections::BTreeSet;

    #[test]
    fn tier_round_trip() {
        for tier in [
            MeshTier::FullyConnected,
            MeshTier::MultiHop,
            MeshTier::NotConnected,
        ] {
            let s = tier.to_string();
            let parsed: MeshTier = s.parse().unwrap();
            assert_eq!(tier, parsed);
        }
    }

    #[test]
    fn tier_from_str_error() {
        let result = "unknown".parse::<MeshTier>();
        assert!(result.is_err());
    }

    #[test]
    fn classify_validator_equal_partitions() {
        // Two partitions of equal size — node in the first must not be
        // misclassified (regression for max_by_key tie-breaking).
        let vc = ValidatorConnectivity {
            topic_name: "/consensus".to_string(),
            all_validators: BTreeSet::from([
                "val1".into(),
                "val2".into(),
                "val3".into(),
                "val4".into(),
            ]),
            actual_partitions: vec![
                BTreeSet::from(["val1".into(), "val2".into()]),
                BTreeSet::from(["val3".into(), "val4".into()]),
            ],
            direct_val_connections: 0,
            max_diameter: 0,
            partition_diameters: vec![],
            completely_isolated: vec![],
            isolated_with_explicit: vec![],
            validators_without_val_peers: vec![],
            indirect_paths: vec![],
        };
        assert_eq!(classify_validator("val1", &vc), MeshTier::FullyConnected);
        assert_eq!(classify_validator("val3", &vc), MeshTier::FullyConnected);
    }
}
