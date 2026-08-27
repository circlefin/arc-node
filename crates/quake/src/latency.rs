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

use color_eyre::eyre::{bail, eyre, Context, OptionExt, Result};
use indexmap::IndexMap;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;
use strum_macros::{Display, EnumString};
use tracing::{debug, trace};

use crate::manifest::Node;
use crate::nodes::NodesMetadata;

/// Filename under `testnet_dir` where node-to-region assignments are persisted.
pub(crate) const REGION_ASSIGNMENTS_FILENAME: &str = "region_assignments.json";

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
    pub(crate) fn index(&self) -> usize {
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

    // Try to load the region assignments file and assign regions to nodes based on it
    let assignments_path = &testnet_dir.join(REGION_ASSIGNMENTS_FILENAME);
    let assignments_path_str = assignments_path.display().to_string();
    if let Ok(region_assignments) = std::fs::read_to_string(assignments_path) {
        let region_assignments =
            serde_json::from_str::<IndexMap<String, String>>(&region_assignments).with_context(
                || format!("Failed to parse region assignments from {assignments_path_str}"),
            )?;
        for (node_name, region) in region_assignments.iter() {
            let node = nodes.get_mut(node_name).ok_or_eyre(format!(
                "Node {node_name} in {REGION_ASSIGNMENTS_FILENAME} not found in manifest"
            ))?;
            if !Region::is_valid(region) {
                bail!("Invalid region {region} in {REGION_ASSIGNMENTS_FILENAME}");
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

/// Read `region_assignments.json` from a testnet's directory and parse each
/// region into a [`Region`]. Used by readiness checks that need to know which
/// region every node was assigned.
pub(crate) fn load_region_assignments(testnet_dir: &Path) -> Result<IndexMap<String, Region>> {
    let path = testnet_dir.join(REGION_ASSIGNMENTS_FILENAME);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(IndexMap::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to read {}", path.display()));
        }
    };
    let parsed: IndexMap<String, String> = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    let mut out = IndexMap::new();
    for (node, region) in parsed {
        let region = Region::from_str(&region).map_err(|_| {
            eyre!(
                "invalid region '{region}' for node '{node}' in {}",
                path.display()
            )
        })?;
        out.insert(node, region);
    }
    Ok(out)
}

/// For each node, build `peer_ip_hex → expected_delay_ms`.
pub(crate) fn build_expected_delays(
    testnet_dir: &Path,
    nodes_metadata: &NodesMetadata,
) -> Result<HashMap<String, HashMap<String, u32>>> {
    let regions = &load_region_assignments(testnet_dir)?;

    let mut out = HashMap::new();
    for (node, region) in regions {
        let mut map = HashMap::new();
        for (peer, peer_region) in regions {
            if peer == node {
                continue;
            }
            let delay = AWS_LATENCY_MATRIX[region.index()][peer_region.index()];
            if delay == 0 {
                continue;
            }
            let mut ips = nodes_metadata.get_consensus_ip_addresses(peer);
            ips.extend(nodes_metadata.get_execution_ip_addresses(peer));
            for ip in ips {
                if let Ok(hex) = ipv4_to_hex(&ip) {
                    map.insert(hex, delay);
                }
            }
        }
        out.insert(node.clone(), map);
    }
    Ok(out)
}

// ── Parsers ────────────────────────────────────────────────────────────

/// Extract `handle → base_delay_ms` from `tc qdisc show` output.
///
/// Sample line we match:
///   `qdisc netem 11: parent 1:11 limit 1000 delay 35ms 1750us 5%`.
pub(crate) fn parse_netem_qdiscs(out: &str) -> BTreeMap<String, u32> {
    let mut result = BTreeMap::new();
    for line in out.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first() != Some(&"qdisc") || tokens.get(1) != Some(&"netem") {
            continue;
        }
        let Some(handle_tok) = tokens.get(2) else {
            continue;
        };
        let handle = handle_tok.trim_end_matches(':').to_string();
        if let Some(idx) = tokens.iter().position(|t| *t == "delay") {
            if let Some(delay_ms) = tokens.get(idx + 1).and_then(parse_delay_ms) {
                result.insert(handle, delay_ms);
            }
        }
    }
    result
}

/// Parse a `delay <N>ms` token. Accepts integer `35ms`; rejects unit-less or
/// non-`ms` forms so `1750us` (the jitter token) doesn't get misread as 1750ms.
pub(crate) fn parse_delay_ms(token: &&str) -> Option<u32> {
    token.strip_suffix("ms").and_then(|n| n.parse::<u32>().ok())
}

/// Extract `peer_ip_hex → handle` from `tc filter show` output.
///
/// `flowid 1:<N>` appears on the filter's leader line; the IP is on the
/// following `match` continuation line as `<hex>/<mask> at 16`.
pub(crate) fn parse_u32_filters(out: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
    let mut current_handle: Option<String> = None;
    for line in out.lines() {
        let trimmed = line.trim();
        if let Some(idx) = trimmed.find("flowid 1:") {
            let rest = &trimmed[idx + "flowid 1:".len()..];
            let handle: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !handle.is_empty() {
                current_handle = Some(handle);
            }
        }
        if let Some(rest) = trimmed.strip_prefix("match ") {
            let mut tokens = rest.split_whitespace();
            let token = tokens.next().unwrap_or("");
            if tokens.next() != Some("at") || tokens.next() != Some("16") {
                continue;
            }
            if let Some((hex, _mask)) = token.split_once('/') {
                if let Some(handle) = current_handle.as_ref() {
                    result.insert(hex.to_string(), handle.clone());
                }
            }
        }
    }
    result
}

/// Convert `"10.0.0.5"` → `"0a000005"` (lowercase hex, big-endian).
fn ipv4_to_hex(ip: &str) -> Result<String> {
    let octets: Vec<u8> = ip
        .split('.')
        .map(|s| {
            s.parse::<u8>()
                .map_err(|_| eyre!("invalid ip octet in {ip}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if octets.len() != 4 {
        return Err(eyre!("ip {ip} is not v4"));
    }
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}",
        octets[0], octets[1], octets[2], octets[3]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_to_hex_basic() {
        assert_eq!(ipv4_to_hex("10.0.0.5").unwrap(), "0a000005");
        assert_eq!(ipv4_to_hex("172.19.1.2").unwrap(), "ac130102");
        assert_eq!(ipv4_to_hex("255.255.255.255").unwrap(), "ffffffff");
    }

    #[test]
    fn ipv4_to_hex_rejects_bad_input() {
        assert!(ipv4_to_hex("not.an.ip.address").is_err());
        assert!(ipv4_to_hex("1.2.3").is_err());
        assert!(ipv4_to_hex("1.2.3.4.5").is_err());
    }

    #[test]
    fn parse_netem_qdiscs_finds_netem_and_skips_others() {
        let out =
            "qdisc htb 1: root refcnt 2 r2q 10 default 0x10 direct_packets_stat 0 direct_qlen 1000
qdisc sfq 10: parent 1:10 limit 127p quantum 1514b depth 127 divisor 1024 perturb 10sec
qdisc netem 11: parent 1:11 limit 1000 delay 35ms 1750us
qdisc netem 12: parent 1:12 limit 1000 delay 50ms 2ms
qdisc netem 13: parent 1:13 limit 1000 delay 120ms
";
        let map = parse_netem_qdiscs(out);
        assert_eq!(map.get("11"), Some(&35));
        assert_eq!(map.get("12"), Some(&50));
        assert_eq!(map.get("13"), Some(&120));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn parse_netem_qdiscs_returns_empty_on_default_qdisc() {
        let out =
            "qdisc pfifo_fast 0: root refcnt 2 bands 3 priomap 1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1\n";
        assert!(parse_netem_qdiscs(out).is_empty());
    }

    #[test]
    fn parse_u32_filters_maps_ip_to_handle() {
        let out = "filter parent 1: protocol ip pref 1 u32 chain 0
filter parent 1: protocol ip pref 1 u32 chain 0 fh 800: ht divisor 1
filter parent 1: protocol ip pref 1 u32 chain 0 fh 800::800 order 2048 key ht 800 bkt 0 flowid 1:11 not_in_hw
  match 0a000005/ffffffff at 16
filter parent 1: protocol ip pref 1 u32 chain 0 fh 800::801 order 2049 key ht 800 bkt 0 flowid 1:12 not_in_hw
  match ac130102/ffffffff at 16
";
        let map = parse_u32_filters(out);
        assert_eq!(map.get("0a000005"), Some(&"11".to_string()));
        assert_eq!(map.get("ac130102"), Some(&"12".to_string()));
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parse_u32_filters_ignores_non_destination_matches() {
        let out = "filter parent 1: protocol ip pref 1 u32 chain 0
filter parent 1: protocol ip pref 1 u32 chain 0 fh 800::800 order 2048 key ht 800 bkt 0 flowid 1:11 not_in_hw
  match 0a000001/ffffffff at 12
  match 0a000005/ffffffff at 16
";
        let map = parse_u32_filters(out);
        assert_eq!(map.get("0a000005"), Some(&"11".to_string()));
        assert!(!map.contains_key("0a000001"));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn parse_u32_filters_returns_empty_when_no_filters() {
        assert!(parse_u32_filters("").is_empty());
    }
}
