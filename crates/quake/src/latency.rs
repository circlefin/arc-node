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

use color_eyre::eyre::{Context, OptionExt, Result};
use indexmap::IndexMap;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;
use strum_macros::{Display, EnumString};
use tracing::{debug, trace};

use crate::manifest::Node;
use crate::nodes::NodesMetadata;

/// AWS regions enum corresponding to the latency matrix indices
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumString, Display)]
pub enum Region {
    /// Tokyo, Japan
    #[serde(rename = "ap-northeast-1")]
    #[strum(serialize = "ap-northeast-1")]
    ApNortheast1 = 0,
    /// Seoul, South Korea
    #[serde(rename = "ap-northeast-2")]
    #[strum(serialize = "ap-northeast-2")]
    ApNortheast2 = 1,
    /// Mumbai, India
    #[serde(rename = "ap-south-1")]
    #[strum(serialize = "ap-south-1")]
    ApSouth1 = 2,
    /// Singapore
    #[serde(rename = "ap-southeast-1")]
    #[strum(serialize = "ap-southeast-1")]
    ApSoutheast1 = 3,
    /// Sydney, Australia
    #[serde(rename = "ap-southeast-2")]
    #[strum(serialize = "ap-southeast-2")]
    ApSoutheast2 = 4,
    /// Montreal, Canada
    #[serde(rename = "ca-central-1")]
    #[strum(serialize = "ca-central-1")]
    CaCentral1 = 5,
    /// Frankfurt, Germany
    #[serde(rename = "eu-central-1")]
    #[strum(serialize = "eu-central-1")]
    EuCentral1 = 6,
    /// Dublin, Ireland
    #[serde(rename = "eu-west-1")]
    #[strum(serialize = "eu-west-1")]
    EuWest1 = 7,
    /// London, UK
    #[serde(rename = "eu-west-2")]
    #[strum(serialize = "eu-west-2")]
    EuWest2 = 8,
    /// São Paulo, Brazil
    #[serde(rename = "sa-east-1")]
    #[strum(serialize = "sa-east-1")]
    SaEast1 = 9,
    /// N. Virginia, USA
    #[serde(rename = "us-east-1")]
    #[strum(serialize = "us-east-1")]
    UsEast1 = 10,
    /// Ohio, USA
    #[serde(rename = "us-east-2")]
    #[strum(serialize = "us-east-2")]
    UsEast2 = 11,
    /// N. California, USA
    #[serde(rename = "us-west-1")]
    #[strum(serialize = "us-west-1")]
    UsWest1 = 12,
    /// Oregon, USA
    #[serde(rename = "us-west-2")]
    #[strum(serialize = "us-west-2")]
    UsWest2 = 13,
}

impl Region {
    /// Get all available regions
    pub fn all() -> Vec<Region> {
        vec![
            Region::ApNortheast1,
            Region::ApNortheast2,
            Region::ApSouth1,
            Region::ApSoutheast1,
            Region::ApSoutheast2,
            Region::CaCentral1,
            Region::EuCentral1,
            Region::EuWest1,
            Region::EuWest2,
            Region::SaEast1,
            Region::UsEast1,
            Region::UsEast2,
            Region::UsWest1,
            Region::UsWest2,
        ]
    }

    /// Get index for latency matrix
    fn index(&self) -> usize {
        *self as usize
    }

    pub fn is_valid(region: &str) -> bool {
        Region::from_str(region).is_ok()
    }
}

/// AWS latency matrix in milliseconds (one-way latency).
/// Matrix is symmetric with zero diagonal (intra-region uses natural latency).
/// Regions ordered alphabetically by AWS region name.
///
/// Note: Values are ONE-WAY latencies (RTT / 2).
/// Source: https://www.cloudping.co/ (P50 median, 1 month) which reports RTT values.
/// We divide by 2 because `tc netem delay` applies one-way delay per direction.
#[rustfmt::skip]
pub(crate) const AWS_LATENCY_MATRIX: [[u32; 14]; 14] = [
//           ap-ne-1 ap-ne-2 ap-s-1 ap-se-1 ap-se-2 ca-c-1 eu-c-1 eu-w-1 eu-w-2 sa-e-1 us-e-1 us-e-2 us-w-1 us-w-2
/*ap-ne-1*/  [  0,     19,     66,     36,     53,    79,   116,   103,   108,   131,    76,    70,    56,    51], // ap-northeast-1 (Tokyo)
/*ap-ne-2*/  [ 19,      0,     67,     37,     77,    93,   117,   121,   126,   149,    93,    86,    70,    65], // ap-northeast-2 (Seoul)
/*ap-s-1 */  [ 66,     67,      0,     33,     78,    95,    62,    63,    58,   150,    95,   100,   118,   112], // ap-south-1 (Mumbai)
/*ap-se-1*/  [ 36,     37,     33,      0,     48,   115,    82,    88,    84,   165,   109,   103,    88,    83], // ap-southeast-1 (Singapore)
/*ap-se-2*/  [ 53,     77,     78,     48,      0,   100,   127,   129,   133,   157,   101,    94,    70,    71], // ap-southeast-2 (Sydney)
/*ca-c-1 */  [ 79,     93,     95,    115,    100,     0,    48,    36,    41,    64,    10,    15,    41,    32], // ca-central-1 (Montreal)
/*eu-c-1 */  [116,    117,     62,     82,    127,    48,     0,    12,     9,   103,    48,    52,    78,    73], // eu-central-1 (Frankfurt)
/*eu-w-1 */  [103,    121,     63,     88,    129,    36,    12,     0,     8,    90,    36,    41,    67,    61], // eu-west-1 (Dublin)
/*eu-w-2 */  [108,    126,     58,     84,    133,    41,     9,     8,     0,    95,    40,    45,    75,    66], // eu-west-2 (London)
/*sa-e-1 */  [131,    149,    150,    165,    157,    64,   103,    90,    95,     0,    58,    63,    88,    89], // sa-east-1 (Sao Paulo)
/*us-e-1 */  [ 76,     93,     95,    109,    101,    10,    48,    36,    40,    58,     0,    10,    35,    34], // us-east-1 (N. Virginia)
/*us-e-2 */  [ 70,     86,    100,    103,     94,    15,    52,    41,    45,    63,    10,     0,    29,    27], // us-east-2 (Ohio)
/*us-w-1 */  [ 56,     70,    118,     88,     70,    41,    78,    67,    75,    88,    35,    29,     0,    12], // us-west-1 (N. California)
/*us-w-2 */  [ 51,     65,    112,     83,     71,    32,    73,    61,    66,    89,    34,    27,    12,     0], // us-west-2 (Oregon)
];

/// Generate bash script content for network latency emulation using tc
/// The generated script is to be executed in the containers of `node_name`, that belongs to `region`.
///
/// For nodes with multiple network interfaces (bridge nodes), latency rules are applied to all interfaces.
fn generate_tc_script(
    node_name: &String,
    region: &Region,
    node_regions: &IndexMap<String, Region>,
    nodes_metadata: &NodesMetadata,
) -> Result<String> {
    let mut script = String::new();

    // Bash script header
    script.push_str("#!/usr/bin/env bash\n");
    script.push_str("# Traffic control script generated for network latency emulation\n");
    script.push_str(&format!("# Node: {node_name}, Region: {region}\n"));
    script.push_str("# Auto-generated by quake\n\n");

    script.push_str("set -e\n\n");

    // Install iproute2 and iptables if ip or tc commands are missing (e.g. release images)
    script.push_str("if [ -f /etc/debian_version ] && ! which ip tc > /dev/null; then\n");
    script.push_str("  (apt-get update -qq && apt-get install -y -qq --no-install-recommends iproute2 iptables) >/dev/null 2>&1\n");
    script.push_str("fi\n\n");

    // Function to set up tc rules on a single interface
    script.push_str("setup_tc_on_interface() {\n");
    script.push_str("    local IF=$1\n");
    script.push_str("    echo \"Setting up traffic control on interface $IF...\"\n\n");

    // Clear existing qdisc rules on the root of the IF interface
    script.push_str("    # Clear existing qdisc rules\n");
    script.push_str("    tc qdisc del dev $IF root 2> /dev/null || true\n\n");

    // Set up new root qdisc with HTB and default class of 10
    script.push_str("    # Set up HTB qdisc\n");
    script.push_str("    tc qdisc add dev $IF root handle 1: htb default 10\n");

    // Add a root class with identifier 1:1 and a rate limit of 1 gigabit per second
    script.push_str("    tc class add dev $IF parent 1: classid 1:1 htb rate 1gbit quantum 1500\n");

    // Add a default class under the root class with identifier 1:10 and a rate limit of 1 gigabit per second
    script
        .push_str("    tc class add dev $IF parent 1:1 classid 1:10 htb rate 1gbit quantum 1500\n");

    // Add an SFQ qdisc to the default class with handle 10: to manage traffic with fairness
    script.push_str("    tc qdisc add dev $IF parent 1:10 handle 10: sfq perturb 10\n\n");

    // handle must be unique for each rule; start from one higher than last handle used above (10).
    let mut handle = 11;

    // Add filters to direct traffic to appropriate netem qdiscs
    for target_region in node_regions.values().collect::<HashSet<&Region>>() {
        // Get latency from the node's region to the target region (note that the matrix is symmetric).
        let latency = AWS_LATENCY_MATRIX[region.index()][target_region.index()];
        if latency == 0 {
            continue;
        }

        // Assign latency +/- 5% to handle.
        let mut delta = latency / 20;
        if delta == 0 {
            // Zero is not allowed in normal distribution.
            delta = 1;
        }

        script.push_str(&format!(
            "    echo \"Setting up traffic filters for nodes in region {target_region} with latency {latency}ms +- {delta}ms...\"\n"
        ));

        // Add a class with the calculated handle, under the root class, with the specified rate.
        script.push_str(&format!(
            "    tc class add dev $IF parent 1:1 classid 1:{handle} htb rate 1gbit quantum 1500\n",
        ));

        // Add a netem qdisc to simulate the specified delay with normal distribution.
        script.push_str(&format!(
            "    tc qdisc add dev $IF parent 1:{handle} handle {handle}: netem delay {latency}ms {delta}ms distribution normal\n",
        ));

        // Set emulated latency to nodes in the target zone.
        for (other_node_name, other_node_region) in node_regions {
            if *other_node_region != *target_region {
                continue;
            }

            // Get all private IP addresses of the target node.
            let mut other_node_ips = nodes_metadata.get_consensus_ip_addresses(other_node_name);
            other_node_ips.extend(nodes_metadata.get_execution_ip_addresses(other_node_name));

            // Assign latency handles to all private IP addresses of the target node.
            for other_node_ip in other_node_ips {
                script.push_str(&format!(
                   "    tc filter add dev $IF protocol ip parent 1: prio 1 u32 match ip dst {other_node_ip}/32 flowid 1:{handle}\n"
            ));
            }
        }

        handle += 1;
    }

    script.push('\n');
    script.push_str(&format!(
        "    echo \"Traffic control setup complete for interface $IF on node {node_name} in region {region}.\"\n"
    ));
    script.push_str("    echo \"Active qdiscs:\"\n");
    script.push_str("    tc qdisc show dev $IF\n");
    script.push_str("    echo \"Active filters:\"\n");
    script.push_str("    tc filter show dev $IF\n");
    script.push_str("}\n\n");

    // Get all network interfaces (excluding loopback and docker/veth interfaces)
    // This handles both single-interface nodes and bridge nodes with multiple ENIs
    // Note: We strip the @ifN suffix that appears in container environments (e.g., eth0@if2179 -> eth0)
    script.push_str("# Find all relevant network interfaces\n");
    script.push_str("INTERFACES=$(ip -o link show | awk -F': ' '{print $2}' | grep -E '^(eth|ens|eno|enp)' | sed 's/@.*//' || true)\n\n");

    script.push_str("if [ -z \"$INTERFACES\" ]; then\n");
    script.push_str(
        "    echo \"No network interfaces found, falling back to default route interface\"\n",
    );
    script.push_str("    INTERFACES=$(ip -o -4 route show to default | awk '{print $5}')\n");
    script.push_str("fi\n\n");

    script.push_str("echo \"Configuring latency emulation on interfaces: $INTERFACES\"\n\n");

    // Apply tc rules to each interface
    script.push_str("for IF in $INTERFACES; do\n");
    script.push_str("    setup_tc_on_interface $IF\n");
    script.push_str("done\n\n");

    script.push_str(&format!(
        "echo \"Traffic control setup complete for node {node_name} in region {region}.\"\n"
    ));

    Ok(script)
}

/// Generate and save latency scripts for all nodes
pub fn generate_latency_scripts(
    testnet_dir: &Path,
    latency_emulation: &mut bool,
    nodes: &mut IndexMap<String, Node>,
    nodes_metadata: &NodesMetadata,
    seed: u64,
    force: bool,
) -> Result<()> {
    debug!(
        "Generating latency emulation scripts for {} nodes",
        nodes_metadata.num_nodes()
    );

    // Try to load region_assignments.json and assign regions to nodes based on the file
    let assignments_path = &testnet_dir.join("region_assignments.json");
    let assignments_path_str = assignments_path.display().to_string();
    if let Ok(region_assignments) = std::fs::read_to_string(assignments_path) {
        let region_assignments =
            serde_json::from_str::<IndexMap<String, String>>(&region_assignments).with_context(
                || format!("Failed to parse region assignments from {assignments_path_str}"),
            )?;
        for (node_name, region) in region_assignments.iter() {
            let node = nodes.get_mut(node_name).ok_or_eyre(format!(
                "Node {node_name} in region_assignments.json not found in manifest"
            ))?;
            if !Region::is_valid(region) {
                color_eyre::eyre::bail!("Invalid region {region} in region_assignments.json");
            }
            node.region = Some(region.clone());
        }
        trace!("Loaded region assignments from {assignments_path_str}");
    }

    // Assign regions to nodes that don't have one yet
    let node_region_map = assign_regions(nodes, latency_emulation, seed)?;

    // Save region assignments as JSON
    let assignments_json = serde_json::to_string_pretty(&node_region_map)
        .context("Failed to serialize region assignments")?;
    fs::write(assignments_path, assignments_json)
        .with_context(|| format!("Failed to write region assignments: {assignments_path_str}"))?;
    trace!("Saved region assignments: {assignments_path_str}");

    // Generate TC scripts for each node
    for (node, region) in node_region_map.clone() {
        let script_path = testnet_dir.join(&node).join("latency_setup.sh");
        let script_path_str = script_path.display().to_string();

        // Skip if the script already exists
        if !force && script_path.exists() {
            debug!("⏭️ Skipping generating latency script for node {node}");
            continue;
        }

        let mut file = fs::File::create(&script_path)
            .with_context(|| format!("Failed to create script file: {script_path_str}"))?;

        let script_content = generate_tc_script(&node, &region, &node_region_map, nodes_metadata)?;
        file.write_all(script_content.as_bytes())
            .with_context(|| format!("Failed to write script content: {script_path_str}"))?;

        // Make script executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&script_path)
                .with_context(|| format!("Failed to get permissions: {script_path_str}"))?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&script_path, perms)
                .with_context(|| format!("Failed to set permissions: {script_path_str}"))?;
        }

        trace!("Generated latency script: {script_path_str}");
    }

    debug!("✅ Generated latency emulation setup files");

    Ok(())
}

/// Assign regions to nodes based on latency emulation settings
fn assign_regions(
    nodes: &mut IndexMap<String, Node>,
    latency_emulation: &mut bool,
    seed: u64,
) -> Result<IndexMap<String, Region>> {
    // If any node already has a region assigned, enable latency emulation for all nodes
    let has_explicit_regions = nodes.values().any(|node| node.region.is_some());
    if has_explicit_regions {
        *latency_emulation = true;
    }

    // Skip if latency emulation is disabled
    if !*latency_emulation {
        return Ok(IndexMap::new());
    }

    let mut rng = StdRng::seed_from_u64(seed);
    let regions = Region::all();

    // Build node to region map while assigning random regions to nodes that don't have them
    let mut node_regions = IndexMap::new();
    for (name, node) in nodes.iter_mut() {
        if let Some(region) = node.region.as_ref() {
            trace!("Node {name}: Already has region {region}");
            node_regions.insert(name.clone(), Region::from_str(region).unwrap());
        } else {
            let region = *regions.choose(&mut rng).unwrap();
            node.region = Some(region.to_string());
            trace!("Node {name}: Assigned region {region}");
            node_regions.insert(name.clone(), region);
        }
    }

    Ok(node_regions)
}
