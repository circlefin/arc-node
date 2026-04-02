use std::fmt::Write;

use super::types::{
    MeshAnalysis, MeshDisplayOptions, NodeMetricsData, NodeType, TopicAnalysis,
    ValidatorConnectivity, TOPICS,
};

pub fn format_report(analysis: &MeshAnalysis, options: &MeshDisplayOptions) -> String {
    let mut out = String::new();

    // Network summary
    let mut type_parts = Vec::new();
    if analysis.validator_count > 0 {
        type_parts.push(format!(
            "{} validator{}",
            analysis.validator_count,
            if analysis.validator_count == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if analysis.persistent_peer_count > 0 {
        type_parts.push(format!(
            "{} persistent peer{}",
            analysis.persistent_peer_count,
            if analysis.persistent_peer_count == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if analysis.full_node_count > 0 {
        type_parts.push(format!(
            "{} full node{}",
            analysis.full_node_count,
            if analysis.full_node_count == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    let network_line = format!(
        "Network: {} ({} total)\n",
        type_parts.join(", "),
        analysis.node_count
    );

    // -- Counts table --------------------------------------------------------
    if options.show_counts {
        let _ = writeln!(out, "{}", "=".repeat(125));
        let _ = writeln!(out, "Status - mesh peers, connected peers, connections");
        let _ = write!(out, "{network_line}");
        let _ = write!(out, "{}\n\n", "=".repeat(125));
        let _ = writeln!(
            out,
            "{:<15} {:<12} {:<5} {:<5} {:<5} {:<5} {:<6} {:<8} {:<9} {:<6} {:<8} {:<8}",
            "Moniker",
            "Type",
            "Cons",
            "Prop",
            "Live",
            "Expl",
            "Peers",
            "InPeers",
            "OutPeers",
            "Conns",
            "InConns",
            "OutConns"
        );
        let _ = writeln!(out, "{}", "-".repeat(125));

        let mut sorted_nodes: Vec<&NodeMetricsData> = analysis.nodes.iter().collect();
        sorted_nodes.sort_by(|a, b| {
            a.node_type
                .cmp(&b.node_type)
                .reverse()
                .then(a.moniker.cmp(&b.moniker))
        });

        let mut prev_type: Option<NodeType> = None;
        for node in &sorted_nodes {
            if let Some(pt) = prev_type {
                if pt != node.node_type {
                    let _ = writeln!(out, "{}", "-".repeat(125));
                }
            }
            prev_type = Some(node.node_type);

            let c = node.mesh_counts.get("/consensus").copied().unwrap_or(0);
            let p = node
                .mesh_counts
                .get("/proposal_parts")
                .copied()
                .unwrap_or(0);
            let l = node.mesh_counts.get("/liveness").copied().unwrap_or(0);
            let expl = node.explicit_peers.len();

            let _ = writeln!(
                out,
                "{:<15} {:<12} {:<5} {:<5} {:<5} {:<5} {:<6} {:<8} {:<9} {:<6} {:<8} {:<8}",
                node.moniker,
                node.node_type,
                c,
                p,
                l,
                expl,
                node.connected_peers,
                node.inbound_peers,
                node.outbound_peers,
                node.active_connections,
                node.inbound_connections,
                node.outbound_connections,
            );
        }

        // Zero mesh warnings
        if analysis.zero_mesh_warnings.is_empty() {
            let _ = write!(
                out,
                "\n✅ All nodes have non-zero mesh peers on all topics\n"
            );
        } else {
            let _ = write!(
                out,
                "\n⚠️  Warning: The following nodes have ZERO mesh peers on at least one topic:\n\n"
            );
            for (moniker, c, p, l) in &analysis.zero_mesh_warnings {
                let _ = writeln!(out, "  {moniker:<20}  C:{c}  P:{p}  L:{l}");
            }
            let _ = write!(
                out,
                "\nThese nodes are not in the eager-push mesh and rely on IHAVE/IWANT gossip (higher latency).\n"
            );
        }
    } else {
        let _ = write!(out, "{network_line}");
    }

    // -- Mesh topology -------------------------------------------------------
    if options.show_mesh {
        let _ = write!(out, "\n{}\n", "=".repeat(80));
        let _ = writeln!(out, "Mesh Partition Analysis (per Topic)");
        let _ = write!(out, "{}\n\n", "=".repeat(80));

        for ta in &analysis.topic_analyses {
            format_topic_analysis(&mut out, ta);
        }

        // Validator connectivity
        let _ = write!(out, "\n{}\n", "=".repeat(80));
        let _ = writeln!(out, "Validator Mesh Connectivity");
        let _ = write!(out, "{}\n\n", "=".repeat(80));

        for vc in &analysis.validator_connectivity {
            format_validator_connectivity(&mut out, vc);
        }

        // Explicit peering
        let _ = write!(out, "\n{}\n", "=".repeat(80));
        let _ = writeln!(out, "Explicit Peering Status");
        let _ = write!(out, "{}\n\n", "=".repeat(80));
        format_explicit_peering(&mut out, analysis);
    }

    // -- Peers detail --------------------------------------------------------
    if options.show_peers {
        let _ = write!(out, "\n{}\n", "=".repeat(80));
        let _ = writeln!(out, "Detailed Peer Information");
        let _ = write!(out, "{}\n\n", "=".repeat(80));

        for node in &analysis.nodes {
            let _ = writeln!(out, "{}:", node.moniker);

            let c = node.mesh_counts.get("/consensus").copied().unwrap_or(0);
            let p = node
                .mesh_counts
                .get("/proposal_parts")
                .copied()
                .unwrap_or(0);
            let l = node.mesh_counts.get("/liveness").copied().unwrap_or(0);
            let _ = writeln!(
                out,
                "  Mesh Counts: Consensus={c}, Proposals={p}, Liveness={l}"
            );

            if !node.explicit_peers.is_empty() {
                let _ = writeln!(
                    out,
                    "  Explicit Peers (direct delivery): {}",
                    node.explicit_peers.join(", ")
                );
            }

            let _ = writeln!(
                out,
                "  Peers: {} connected, {} inbound, {} outbound",
                node.connected_peers, node.inbound_peers, node.outbound_peers
            );
            let _ = writeln!(
                out,
                "  Connections: {} active, {} inbound, {} outbound",
                node.active_connections, node.inbound_connections, node.outbound_connections
            );

            // Mesh peers per topic
            for &topic in &TOPICS {
                if let Some(peers) = node.mesh_peers.get(topic) {
                    if !peers.is_empty() {
                        let mut sorted = peers.clone();
                        sorted.sort();
                        let _ = writeln!(out, "  Mesh peers ({topic}): {}", sorted.join(", "));
                    }
                }
            }

            // Full peer detail (peer type + score)
            if options.show_peers_full && !node.discovered_peers.is_empty() {
                let _ = writeln!(out, "  Discovered peers:");
                for dp in node.discovered_peers.values() {
                    let _ = writeln!(
                        out,
                        "    {:<20} type={:<16} score={:.1}",
                        dp.peer_moniker, dp.peer_type, dp.score,
                    );
                }
            }

            let _ = writeln!(out);
        }
    }

    let _ = write!(out, "\n{}\n\n", "=".repeat(80));
    out
}

fn format_topic_analysis(out: &mut String, ta: &TopicAnalysis) {
    let total = ta.meshed_count + ta.isolated_count;

    if ta.meshed_count == 0 {
        let _ = writeln!(out, "  No nodes participating in {} topic", ta.topic_name);
        return;
    }

    if ta.partitions.len() == 1 && ta.isolated_count == 0 {
        let _ = writeln!(
            out,
            "  ✅ {}: fully connected ({total} nodes in single mesh)",
            ta.topic_name
        );
    } else {
        if ta.isolated_count > 0 && ta.partitions.len() == 1 {
            let _ = writeln!(
                out,
                "  ⚠️  {}: {} of {total} nodes meshed ({} isolated)",
                ta.topic_name, ta.meshed_count, ta.isolated_count
            );
        } else if ta.partitions.len() > 1 {
            let _ = writeln!(
                out,
                "  ⚠️  {}: partitioned into {} groups ({} total nodes)",
                ta.topic_name,
                ta.partitions.len(),
                ta.meshed_count
            );
        }

        for (idx, partition) in ta.partitions.iter().enumerate() {
            let nodes: Vec<&String> = partition.iter().collect();
            let _ = writeln!(
                out,
                "     group {}: {} nodes - {}",
                idx + 1,
                nodes.len(),
                nodes
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        if ta.isolated_count > 0 {
            let mut sorted = ta.isolated_nodes.clone();
            sorted.sort();
            let _ = writeln!(out, "     isolated: {}", sorted.join(", "));
        }
    }
}

fn format_validator_connectivity(out: &mut String, vc: &ValidatorConnectivity) {
    if vc.all_validators.is_empty() {
        let _ = writeln!(out, "  ℹ️  {}: no validators found in mesh", vc.topic_name);
        return;
    }

    let num_validators = vc.all_validators.len();
    let num_partitions = vc.actual_partitions.len();

    if num_partitions > 1 {
        let _ = writeln!(
            out,
            "  ⚠️  {}: {num_validators} validators PARTITIONED into {num_partitions} mesh groups (must use IHAVE/IWANT)",
            vc.topic_name
        );
        let diameter_strs: Vec<String> = vc
            .partition_diameters
            .iter()
            .enumerate()
            .map(|(i, d)| {
                format!(
                    "P{}={}",
                    i + 1,
                    d.map(|v| format!("{v} hops")).unwrap_or("N/A".to_string())
                )
            })
            .collect();
        let _ = writeln!(
            out,
            "     Network diameter per partition: {}",
            diameter_strs.join(", ")
        );
        let _ = writeln!(
            out,
            "     Direct validator-to-validator connections: {}",
            vc.direct_val_connections
        );
    } else {
        if vc.max_diameter > 1 {
            let _ = writeln!(
                out,
                "  ⚠️  {}: {num_validators} validators in single mesh (eager push works, NOT fully meshed)",
                vc.topic_name
            );
        } else {
            let _ = writeln!(
                out,
                "  ✅ {}: {num_validators} validators in single mesh (eager push works, fully meshed)",
                vc.topic_name
            );
        }
        let _ = writeln!(
            out,
            "     Network diameter: {} hops (max distance between any two validators)",
            vc.max_diameter
        );
        let _ = writeln!(
            out,
            "     Direct validator-to-validator mesh connections: {}",
            vc.direct_val_connections
        );
    }

    // Completely isolated validators
    if !vc.completely_isolated.is_empty() {
        let _ = writeln!(
            out,
            "     🚨 CRITICAL: Validators with ZERO mesh peers (NOT receiving eager push):"
        );
        for v in &vc.completely_isolated {
            let _ = writeln!(out, "       {v}");
        }
    }

    // Isolated with explicit peers
    if !vc.isolated_with_explicit.is_empty() {
        let _ = writeln!(
            out,
            "     ℹ️  Validators using explicit peering (bypassing mesh, direct delivery):"
        );
        for (v, peers) in &vc.isolated_with_explicit {
            let mut sorted = peers.clone();
            sorted.sort();
            let _ = writeln!(out, "       {v} → explicit peers: {}", sorted.join(", "));
        }
    }

    // Validators without direct validator mesh peers
    if !vc.validators_without_val_peers.is_empty() {
        let _ = writeln!(
            out,
            "     Validators without direct validator mesh connections (meshed with full nodes only):"
        );
        for v in &vc.validators_without_val_peers {
            let _ = writeln!(out, "       {v}");
        }
    }

    // Indirect paths
    if !vc.indirect_paths.is_empty() {
        let _ = writeln!(
            out,
            "     Indirect paths (persistent peers communicating via full nodes):"
        );
        for (v1, v2, intermediate, hops) in &vc.indirect_paths {
            let _ = writeln!(
                out,
                "       {v1} → {v2}: via [{}] ({hops} hops)",
                intermediate.join(", ")
            );
        }
    }
}

fn format_explicit_peering(out: &mut String, analysis: &MeshAnalysis) {
    let validators: Vec<&NodeMetricsData> = analysis
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::Validator)
        .collect();

    let full_nodes: Vec<&NodeMetricsData> = analysis
        .nodes
        .iter()
        .filter(|n| n.node_type == NodeType::FullNode)
        .collect();

    let validators_with_explicit: Vec<(&str, &[String])> = validators
        .iter()
        .filter(|n| !n.explicit_peers.is_empty())
        .map(|n| (n.moniker.as_str(), n.explicit_peers.as_slice()))
        .collect();

    let validators_without_explicit: Vec<&str> = validators
        .iter()
        .filter(|n| n.explicit_peers.is_empty())
        .map(|n| n.moniker.as_str())
        .collect();

    let fullnodes_with_explicit: Vec<(&str, &[String])> = full_nodes
        .iter()
        .filter(|n| !n.explicit_peers.is_empty())
        .map(|n| (n.moniker.as_str(), n.explicit_peers.as_slice()))
        .collect();

    if !validators_with_explicit.is_empty() {
        let _ = write!(
            out,
            "  ✅ Explicit peering ENABLED - {} validators using direct delivery\n\n",
            validators_with_explicit.len()
        );
        let _ = writeln!(
            out,
            "  Validators with explicit peers (bypassing mesh for direct delivery):"
        );
        let mut sorted = validators_with_explicit;
        sorted.sort_by_key(|(name, _)| *name);
        for (name, peers) in &sorted {
            let mut p: Vec<&str> = peers.iter().map(|s| s.as_str()).collect();
            p.sort();
            let _ = writeln!(out, "    {name} → {}", p.join(", "));
        }

        if !fullnodes_with_explicit.is_empty() {
            let _ = write!(
                out,
                "\n  Full nodes with explicit peers ({} nodes):\n",
                fullnodes_with_explicit.len()
            );
            let mut sorted = fullnodes_with_explicit;
            sorted.sort_by_key(|(name, _)| *name);
            for (name, peers) in &sorted {
                let mut p: Vec<&str> = peers.iter().map(|s| s.as_str()).collect();
                p.sort();
                let _ = writeln!(out, "    {name} → {}", p.join(", "));
            }
        }

        let _ = write!(
            out,
            "\n  ℹ️  With explicit peering, mesh partitioning warnings above may not indicate\n"
        );
        let _ = writeln!(
            out,
            "     a problem - validators communicate directly outside the mesh."
        );
    } else {
        let _ = write!(
            out,
            "  ℹ️  Explicit peering NOT enabled (or no explicit peers connected)\n\n"
        );
        if !validators_without_explicit.is_empty() {
            let mut sorted = validators_without_explicit;
            sorted.sort();
            let _ = write!(
                out,
                "  Validators relying on mesh only: {}\n\n",
                sorted.join(", ")
            );

            // Only warn if there are actual partitioning issues above
            let has_partition_warnings =
                analysis.topic_analyses.iter().any(|ta| {
                    ta.partitions.len() > 1 || ta.isolated_count > 0 || ta.meshed_count == 0
                }) || analysis.validator_connectivity.iter().any(|vc| {
                    vc.actual_partitions.len() > 1
                        || !vc.completely_isolated.is_empty()
                        || !vc.validators_without_val_peers.is_empty()
                });

            if has_partition_warnings {
                let _ = writeln!(
                    out,
                    "  ⚠️  Mesh partitioning warnings above ARE significant - validators need"
                );
                let _ = writeln!(
                    out,
                    "     mesh connectivity or IHAVE/IWANT gossip for message delivery."
                );
            } else {
                let _ = writeln!(
                    out,
                    "  ✅ All validators fully meshed - no partitioning concerns."
                );
            }
        }
    }
}
