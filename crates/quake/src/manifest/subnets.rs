use color_eyre::eyre::{bail, Result};
use indexmap::IndexMap;
use serde::Serialize;
use std::collections::HashMap;

use crate::node::{CidrBlock, NodeName, SubnetName};

/// Subnet index offset: user-defined subnets start at 172.21.x.x to avoid
/// conflicting with the base Docker network at 172.19.x.x / 172.20.x.x.
const SUBNET_INDEX_BASE: usize = 21;

/// Subnets derived from the manifest.
#[derive(Serialize, Clone, Debug, PartialEq, Default)]
pub(crate) struct Subnets {
    subnets_meta: IndexMap<SubnetName, SubnetMetadata>,
    subnet_nodes: IndexMap<SubnetName, Vec<NodeName>>,
    node_subnets: IndexMap<NodeName, Vec<SubnetName>>,
}

/// Metadata for a single subnet in the testnet topology.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub(crate) struct SubnetMetadata {
    /// The name of the subnet in the manifest
    pub name: SubnetName,
    /// The order of first appearance of the subnet in the manifest
    pub index: usize,
    /// The CIDR block of the subnet
    pub cidr: CidrBlock,
}

impl Subnets {
    /// Build a `Subnets` object from the manifest's node definitions.
    ///
    /// Assigns each subnet a unique index (second octet starting at
    /// [`SUBNET_INDEX_BASE`]), computes CIDR blocks, and validates that
    /// the resulting network graph is connected.
    pub fn new(node_subnets: &IndexMap<NodeName, Vec<SubnetName>>) -> Self {
        // Build a map of subnet names to the nodes that are connected to them
        let mut subnet_nodes: IndexMap<SubnetName, Vec<NodeName>> = IndexMap::new();
        for (node_name, node) in node_subnets.iter() {
            for subnet in node.iter() {
                subnet_nodes
                    .entry(subnet.clone())
                    .or_default()
                    .push(node_name.clone());
            }
        }

        // Build a map of subnet names to the metadata for the subnet
        let subnets_meta = subnet_nodes
            .iter()
            .enumerate()
            .map(|(position, (name, _))| {
                let index = position + SUBNET_INDEX_BASE;
                (
                    name.clone(),
                    SubnetMetadata {
                        name: name.clone(),
                        index,
                        cidr: format!("172.{index}.0.0/16"),
                    },
                )
            })
            .collect::<IndexMap<_, _>>();

        Self {
            subnets_meta,
            subnet_nodes,
            node_subnets: node_subnets.clone(),
        }
    }

    /// CIDR map for the compose template (subnet name -> CIDR block).
    pub fn cidr_map(&self) -> IndexMap<SubnetName, CidrBlock> {
        self.subnets_meta
            .iter()
            .map(|(name, meta)| (name.clone(), meta.cidr.clone()))
            .collect()
    }

    /// The subnet indexes for a given node, as `(SubnetName, index)` pairs.
    ///
    /// The index is the second octet used when building private IP addresses.
    pub fn subnet_indexes_for(&self, node: &str) -> Vec<(SubnetName, usize)> {
        self.node_subnets
            .get(node)
            .map(|subnet_names| {
                subnet_names
                    .iter()
                    .filter_map(|s| self.subnets_meta.get(s).map(|meta| (s.clone(), meta.index)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Subnet names shared between two nodes.
    pub fn shared_subnets(&self, node_a: &str, node_b: &str) -> Vec<SubnetName> {
        let a_subnets = self
            .node_subnets
            .get(node_a)
            .unwrap_or_else(|| panic!("node {node_a} not found in manifest"));
        let b_subnets = self
            .node_subnets
            .get(node_b)
            .unwrap_or_else(|| panic!("node {node_b} not found in manifest"));
        a_subnets
            .iter()
            .filter(|s| b_subnets.contains(s))
            .cloned()
            .collect()
    }

    /// Subnets a node is connected to.
    pub fn subnets_of(&self, node: &str) -> Vec<SubnetName> {
        self.node_subnets
            .get(node)
            .cloned()
            .unwrap_or_else(|| panic!("node {node} not found in subnets"))
    }

    /// Validates that the network topology forms a connected graph.
    ///
    /// Uses a Union-Find-like approach: starts with each subnet as a separate
    /// component, then iteratively merges subnets that share a bridge node (a
    /// node belonging to multiple subnets). If all subnets collapse into a
    /// single component, the network is connected; otherwise, it is disconnected.
    pub fn validate_topology(&self) -> Result<()> {
        if self.subnets_meta.len() <= 1 {
            return Ok(());
        }

        let mut working_map: IndexMap<SubnetName, Vec<NodeName>> = self.subnet_nodes.clone();

        let find_destination =
            |subnet: &str, removed: &HashMap<SubnetName, SubnetName>| -> Option<String> {
                let mut current = subnet.to_string();
                while let Some(dest) = removed.get(&current) {
                    current = dest.clone();
                }
                if current == subnet {
                    None
                } else {
                    Some(current)
                }
            };

        let mut removed_subnets: HashMap<SubnetName, SubnetName> = HashMap::new();

        for (_, subnets) in self.node_subnets.iter() {
            if subnets.len() <= 1 {
                continue;
            }

            let first_subnet = subnets.first().unwrap();
            let other_subnets = subnets.iter().skip(1).collect::<Vec<&SubnetName>>();
            for subnet in other_subnets {
                let Some(other_nodes) = working_map.swap_remove(subnet) else {
                    // This subnet was already merged. Find where it ended up and
                    // merge first_subnet's component into that destination so the
                    // transitive connection is preserved.
                    let other_dest = find_destination(subnet, &removed_subnets)
                        .unwrap_or_else(|| subnet.to_string());
                    let first_dest = find_destination(first_subnet, &removed_subnets)
                        .unwrap_or_else(|| first_subnet.to_string());

                    if other_dest == first_dest {
                        continue;
                    }

                    let first_nodes = working_map
                        .swap_remove(&first_dest)
                        .expect("destination subnet must be in the map");
                    removed_subnets.insert(first_dest, other_dest.clone());
                    working_map
                        .get_mut(&other_dest)
                        .expect("destination subnet must be in the map")
                        .extend(first_nodes);

                    continue;
                };

                let destination = find_destination(first_subnet, &removed_subnets)
                    .unwrap_or_else(|| first_subnet.to_string());

                removed_subnets.insert(subnet.clone(), destination.clone());

                if let Some(dest_nodes) = working_map.get_mut(&destination) {
                    dest_nodes.extend(other_nodes);
                } else {
                    bail!("Destination subnet {destination} not found; cannot merge subnets");
                }
            }
        }

        if working_map.len() > 1 {
            bail!("Network topology is disconnected: {working_map:?}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_subnet_is_connected() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["default".to_string()]),
            ("node2".to_string(), vec!["default".to_string()]),
            ("node3".to_string(), vec!["default".to_string()]),
        ]
        .into();
        assert!(Subnets::new(&node_subnets).validate_topology().is_ok());
    }

    #[test]
    fn two_subnets_with_bridge_is_connected() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node3".to_string(), vec!["B".to_string()]),
        ]
        .into();
        assert!(Subnets::new(&node_subnets).validate_topology().is_ok());
    }

    #[test]
    fn chain_topology_is_connected() {
        // A -- B -- C -- D (chain of subnets)
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node3".to_string(), vec!["B".to_string()]),
            ("node4".to_string(), vec!["B".to_string(), "C".to_string()]),
            ("node5".to_string(), vec!["C".to_string()]),
            ("node6".to_string(), vec!["C".to_string(), "D".to_string()]),
            ("node7".to_string(), vec!["D".to_string()]),
        ]
        .into();
        assert!(Subnets::new(&node_subnets).validate_topology().is_ok());
    }

    #[test]
    fn star_topology_is_connected() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            (
                "center".to_string(),
                vec![
                    "A".to_string(),
                    "B".to_string(),
                    "C".to_string(),
                    "D".to_string(),
                ],
            ),
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["B".to_string()]),
            ("node3".to_string(), vec!["C".to_string()]),
            ("node4".to_string(), vec!["D".to_string()]),
        ]
        .into();
        assert!(Subnets::new(&node_subnets).validate_topology().is_ok());
    }

    #[test]
    fn two_disconnected_subnets_fails() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["A".to_string()]),
            ("node3".to_string(), vec!["B".to_string()]),
            ("node4".to_string(), vec!["B".to_string()]),
        ]
        .into();
        assert!(Subnets::new(&node_subnets).validate_topology().is_err());
    }

    #[test]
    fn partial_disconnection_fails() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node3".to_string(), vec!["B".to_string()]),
            ("node4".to_string(), vec!["C".to_string()]),
        ]
        .into();
        assert!(Subnets::new(&node_subnets).validate_topology().is_err());
    }

    #[test]
    fn multiple_bridges_same_subnets() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node3".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node4".to_string(), vec!["B".to_string()]),
        ]
        .into();
        assert!(Subnets::new(&node_subnets).validate_topology().is_ok());
    }

    #[test]
    fn test_chain_via_shared_middle_subnet_is_connected() {
        // Bridges ["A","B"] and ["C","B"] form a chain A–B–C.
        // After the first bridge merges B into A, the second bridge finds B
        // already removed and must propagate the merge so C joins A.
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("n1".to_string(), vec!["A".to_string()]),
            (
                "bridge_ab".to_string(),
                vec!["A".to_string(), "B".to_string()],
            ),
            ("n2".to_string(), vec!["B".to_string()]),
            (
                "bridge_cb".to_string(),
                vec!["C".to_string(), "B".to_string()],
            ),
            ("n3".to_string(), vec!["C".to_string()]),
        ]
        .into();

        assert!(Subnets::new(&node_subnets).validate_topology().is_ok());
    }

    #[test]
    fn cidr_map_returns_correct_blocks() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node3".to_string(), vec!["B".to_string()]),
        ]
        .into();
        let subnets = Subnets::new(&node_subnets);
        let cidr = subnets.cidr_map();
        assert_eq!(cidr.get("A").unwrap(), "172.21.0.0/16");
        assert_eq!(cidr.get("B").unwrap(), "172.22.0.0/16");
    }

    #[test]
    fn subnet_indexes_for_returns_correct_indexes() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node3".to_string(), vec!["B".to_string()]),
        ]
        .into();
        let subnets = Subnets::new(&node_subnets);
        assert_eq!(
            subnets.subnet_indexes_for("node2"),
            vec![("A".to_string(), 21), ("B".to_string(), 22)]
        );
    }

    #[test]
    fn shared_subnets_returns_common_subnets() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node3".to_string(), vec!["B".to_string()]),
        ]
        .into();
        let subnets = Subnets::new(&node_subnets);
        assert_eq!(subnets.shared_subnets("node1", "node2"), vec!["A"]);
        assert!(subnets.shared_subnets("node1", "node3").is_empty());
    }

    #[test]
    fn shared_subnets_multiple_common() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            (
                "node1".to_string(),
                vec!["A".to_string(), "B".to_string(), "C".to_string()],
            ),
            (
                "node2".to_string(),
                vec!["A".to_string(), "B".to_string(), "D".to_string()],
            ),
            ("node3".to_string(), vec!["D".to_string()]),
        ]
        .into();
        let subnets = Subnets::new(&node_subnets);
        let shared = subnets.shared_subnets("node1", "node2");
        assert_eq!(shared.len(), 2);
        assert!(shared.contains(&"A".to_string()));
        assert!(shared.contains(&"B".to_string()));
    }

    #[test]
    fn shared_subnets_order_invariant() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node2".to_string(), vec!["A".to_string(), "B".to_string()]),
        ]
        .into();
        let subnets = Subnets::new(&node_subnets);
        assert_eq!(
            subnets.shared_subnets("node1", "node2"),
            subnets.shared_subnets("node2", "node1")
        );
    }

    #[test]
    fn shared_subnets_same_node_returns_all_subnets() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string(), "B".to_string()]),
            ("node2".to_string(), vec!["A".to_string()]),
        ]
        .into();
        let subnets = Subnets::new(&node_subnets);
        let shared = subnets.shared_subnets("node1", "node1");
        assert_eq!(shared, vec!["A".to_string(), "B".to_string()]);
    }

    #[test]
    #[should_panic(expected = "node nonexistent not found in manifest")]
    fn shared_subnets_panics_on_unknown_node() {
        let node_subnets: IndexMap<NodeName, Vec<SubnetName>> = [
            ("node1".to_string(), vec!["A".to_string()]),
            ("node2".to_string(), vec!["A".to_string()]),
        ]
        .into();
        let subnets = Subnets::new(&node_subnets);
        subnets.shared_subnets("node1", "nonexistent");
    }
}
