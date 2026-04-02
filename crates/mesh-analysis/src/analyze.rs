use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use super::types::{
    MeshAnalysis, NodeMetricsData, NodeType, TopicAnalysis, ValidatorConnectivity, TOPICS,
};

fn find_partitions(
    graph: &HashMap<String, Vec<String>>,
    all_nodes: &[String],
) -> Vec<BTreeSet<String>> {
    let node_set: HashSet<&String> = all_nodes.iter().collect();
    let mut visited = HashSet::new();
    let mut partitions = Vec::new();

    for node in all_nodes {
        if visited.contains(node) {
            continue;
        }
        let mut partition = BTreeSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(node.clone());

        while let Some(current) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            partition.insert(current.clone());
            if let Some(neighbors) = graph.get(&current) {
                for neighbor in neighbors {
                    if !visited.contains(neighbor) && node_set.contains(neighbor) {
                        queue.push_back(neighbor.clone());
                    }
                }
            }
        }
        partitions.push(partition);
    }
    partitions
}

fn find_shortest_path(graph: &HashMap<String, Vec<String>>, start: &str, end: &str) -> Vec<String> {
    if start == end {
        return vec![start.to_string()];
    }
    let mut visited = HashSet::new();
    let mut queue: VecDeque<(String, Vec<String>)> = VecDeque::new();
    queue.push_back((start.to_string(), vec![start.to_string()]));

    while let Some((current, path)) = queue.pop_front() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if let Some(neighbors) = graph.get(&current) {
            for neighbor in neighbors {
                if neighbor == end {
                    let mut full = path.clone();
                    full.push(neighbor.clone());
                    return full;
                }
                if !visited.contains(neighbor) {
                    let mut new_path = path.clone();
                    new_path.push(neighbor.clone());
                    queue.push_back((neighbor.clone(), new_path));
                }
            }
        }
    }
    vec![] // no path
}

pub fn analyze(nodes: &[NodeMetricsData]) -> MeshAnalysis {
    let node_count = nodes.len();
    let validator_count = nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Validator)
        .count();
    let persistent_peer_count = nodes
        .iter()
        .filter(|n| n.node_type == NodeType::PersistentPeer)
        .count();
    let full_node_count = nodes
        .iter()
        .filter(|n| n.node_type == NodeType::FullNode)
        .count();

    let mut topic_analyses = Vec::new();
    let mut validator_connectivity = Vec::new();

    for &topic in &TOPICS {
        // Topic partition analysis
        let mut graph: HashMap<String, Vec<String>> = HashMap::new();
        let mut meshed_nodes = Vec::new();
        let mut isolated_nodes = Vec::new();

        for node in nodes {
            let count = node.mesh_counts.get(topic).copied().unwrap_or(0);
            if count > 0 {
                let peers = node.mesh_peers.get(topic).cloned().unwrap_or_default();
                graph.insert(node.moniker.clone(), peers);
                meshed_nodes.push(node.moniker.clone());
            } else {
                isolated_nodes.push(node.moniker.clone());
            }
        }

        let partitions = if meshed_nodes.is_empty() {
            vec![]
        } else {
            find_partitions(&graph, &meshed_nodes)
        };

        topic_analyses.push(TopicAnalysis {
            topic_name: topic.to_string(),
            meshed_count: meshed_nodes.len(),
            isolated_count: isolated_nodes.len(),
            isolated_nodes,
            partitions,
        });

        // Validator connectivity analysis
        let all_validators: BTreeSet<String> = nodes
            .iter()
            .filter(|n| n.node_type == NodeType::Validator)
            .map(|n| n.moniker.clone())
            .collect();

        if all_validators.is_empty() {
            continue;
        }

        // Build complete mesh graph (all nodes) and validator-only graph
        let mut complete_mesh: HashMap<String, Vec<String>> = HashMap::new();
        let mut validator_mesh: HashMap<String, Vec<String>> = HashMap::new();

        for node in nodes {
            let peers = node.mesh_peers.get(topic).cloned().unwrap_or_default();
            complete_mesh.insert(node.moniker.clone(), peers.clone());

            if all_validators.contains(&node.moniker) {
                let val_peers: Vec<String> = peers
                    .iter()
                    .filter(|p| all_validators.contains(*p))
                    .cloned()
                    .collect();
                validator_mesh.insert(node.moniker.clone(), val_peers);
            }
        }

        // Find actual mesh partitions using complete graph
        let all_meshed: Vec<String> = complete_mesh.keys().cloned().collect();
        let all_mesh_partitions = find_partitions(&complete_mesh, &all_meshed);

        let actual_partitions: Vec<BTreeSet<String>> = all_mesh_partitions
            .iter()
            .filter_map(|partition| {
                let vals: BTreeSet<String> = partition
                    .iter()
                    .filter(|v| all_validators.contains(*v))
                    .cloned()
                    .collect();
                if vals.is_empty() {
                    None
                } else {
                    Some(vals)
                }
            })
            .collect();

        // Direct val-to-val connections: divide by 2 because gossipsub meshes are
        // bidirectional — if A lists B as a mesh peer, B also lists A. If metrics
        // scraping is asymmetric (e.g. stale data on one node), the sum may be odd
        // and integer division will slightly undercount.
        let direct_val_connections: usize =
            validator_mesh.values().map(|p| p.len()).sum::<usize>() / 2;

        // Diameter per partition
        let mut partition_diameters = Vec::new();
        for partition in &actual_partitions {
            if partition.len() <= 1 {
                partition_diameters.push(None);
                continue;
            }
            let mut max_hops = 0usize;
            let vals: Vec<&String> = partition.iter().collect();
            for (i, v1) in vals.iter().enumerate() {
                for v2 in &vals[i + 1..] {
                    let path = find_shortest_path(&complete_mesh, v1, v2);
                    if !path.is_empty() {
                        max_hops = max_hops.max(path.len() - 1);
                    }
                }
            }
            partition_diameters.push(if max_hops > 0 { Some(max_hops) } else { None });
        }

        let max_diameter = partition_diameters
            .iter()
            .filter_map(|d| *d)
            .max()
            .unwrap_or(0);

        // Completely isolated validators (zero mesh peers)
        let mut completely_isolated = Vec::new();
        let mut isolated_with_explicit = Vec::new();
        for v in all_validators.iter() {
            let peer_count = complete_mesh.get(v).map(|p| p.len()).unwrap_or(0);
            if peer_count == 0 {
                let node = nodes.iter().find(|n| &n.moniker == v);
                if let Some(node) = node {
                    if node.explicit_peers.is_empty() {
                        completely_isolated.push(v.clone());
                    } else {
                        isolated_with_explicit.push((v.clone(), node.explicit_peers.clone()));
                    }
                } else {
                    completely_isolated.push(v.clone());
                }
            }
        }

        // Validators with no direct validator mesh peers but with some mesh peers
        let validators_without_val_peers: Vec<String> = all_validators
            .iter()
            .filter(|v| {
                validator_mesh.get(*v).map(|p| p.is_empty()).unwrap_or(true)
                    && complete_mesh
                        .get(*v)
                        .map(|p| !p.is_empty())
                        .unwrap_or(false)
            })
            .cloned()
            .collect();

        // Indirect paths between validators (through full nodes)
        let mut indirect_paths = Vec::new();
        if actual_partitions.len() == 1 {
            let vals: Vec<&String> = all_validators.iter().collect();
            for (i, v1) in vals.iter().enumerate() {
                for v2 in &vals[i + 1..] {
                    // Skip if directly connected
                    if validator_mesh
                        .get(*v1)
                        .map(|p| p.contains(*v2))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let path = find_shortest_path(&complete_mesh, v1, v2);
                    if path.len() > 2 {
                        let intermediate: Vec<String> = path[1..path.len() - 1].to_vec();
                        let hops = path.len() - 1;
                        indirect_paths.push(((*v1).clone(), (*v2).clone(), intermediate, hops));
                    }
                }
            }
            indirect_paths.sort_by(|a, b| a.3.cmp(&b.3).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
        }

        validator_connectivity.push(ValidatorConnectivity {
            topic_name: topic.to_string(),
            all_validators: all_validators.clone(),
            actual_partitions,
            direct_val_connections,
            max_diameter,
            partition_diameters,
            completely_isolated,
            isolated_with_explicit,
            validators_without_val_peers,
            indirect_paths,
        });
    }

    // Zero-mesh warnings
    let zero_mesh_warnings: Vec<(String, i64, i64, i64)> = nodes
        .iter()
        .filter_map(|n| {
            let c = n.mesh_counts.get("/consensus").copied().unwrap_or(0);
            let p = n.mesh_counts.get("/proposal_parts").copied().unwrap_or(0);
            let l = n.mesh_counts.get("/liveness").copied().unwrap_or(0);
            if c == 0 || p == 0 || l == 0 {
                Some((n.moniker.clone(), c, p, l))
            } else {
                None
            }
        })
        .collect();

    MeshAnalysis {
        node_count,
        validator_count,
        persistent_peer_count,
        full_node_count,
        nodes: nodes.to_vec(),
        topic_analyses,
        validator_connectivity,
        zero_mesh_warnings,
    }
}
