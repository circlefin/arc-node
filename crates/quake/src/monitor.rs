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

//! Remote health monitoring — SSH resource collection and live dashboard.

use color_eyre::eyre::{self, Result};
use reqwest::Url;
use std::collections::{HashMap, HashSet};
use tokio::time::Duration;

use crate::infra::remote::{self, RemoteInfra};
use crate::infra::InfraData;
use crate::node::{IpAddress, NodeName};
use crate::rpc;

const CC_TITLE: &str = "Control Center";
const REFRESH_INTERVAL_SINGLE_HOST: u64 = 5;
const REFRESH_INTERVAL_MULTI_HOST: u64 = 30;

// ── Types ────────────────────────────────────────────────────────────────────

type SshTargets = Vec<(NodeName, IpAddress)>;
type HostUsageMap = HashMap<IpAddress, Result<HostUsage>>;

struct HostUsage {
    mem_total: String,
    mem_used: String,
    cpu_percent: String,
    cpu_pct: f64,
    disk_total: String,
    disk_used: String,
    containers: Vec<ContainerUsage>,
}

struct ContainerUsage {
    name: String,
    mem_usage: String,
    mem_limit: String,
}

struct RpcSnapshot {
    height: Result<u64>,
    peer_count: Result<usize>,
}

// ── Public entry points ──────────────────────────────────────────────────────

/// Monitor remote testnet health.
///
/// When `follow` is true, continuously refreshes: single-host mode appends
/// time-series rows, multi-host mode redraws the dashboard each tick.
/// When `follow` is false, collects and prints data once, then returns.
pub(crate) async fn monitor_loop(
    infra_data: &InfraData,
    remote: &RemoteInfra,
    node_urls: &[(String, Url)],
    node_or_cc: &str,
    follow: bool,
    interval: Option<u64>,
) -> Result<()> {
    let targets = build_targets(infra_data, node_or_cc);
    if targets.is_empty() {
        println!("No instances to monitor");
        return Ok(());
    }

    let target_names: HashSet<&str> = targets.iter().map(|(name, _)| name.as_str()).collect();
    let filtered_urls: Vec<_> = node_urls
        .iter()
        .filter(|(name, _)| target_names.contains(name.as_str()))
        .cloned()
        .collect();

    let single_host = targets.len() == 1;
    let interval = interval.unwrap_or(if single_host {
        REFRESH_INTERVAL_SINGLE_HOST
    } else {
        REFRESH_INTERVAL_MULTI_HOST
    });
    let mut container_names: Vec<String> = Vec::new();
    let mut header_printed = false;

    loop {
        let ssh_data = match collect_ssh_usage(&targets, remote) {
            Ok(data) => data,
            Err(e) if follow => {
                eprintln!("Stopped during SSH collection: {e}");
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let rpc_data = collect_rpc_data(&filtered_urls).await;

        if single_host {
            let is_cc = is_cc(&targets[0].0);
            if !header_printed {
                if let Some(Ok(u)) = ssh_data.get(&targets[0].1) {
                    container_names = u.containers.iter().map(|c| c.name.clone()).collect();
                }
                print_single_host_header(is_cc, &container_names);
                header_printed = true;
            }
            print_single_host_row(&targets[0], &ssh_data, &rpc_data, is_cc, &container_names);
        } else {
            if follow {
                print!("\x1b[2J\x1b[H");
                let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
                println!("Remote Health Monitor ({interval}s refresh)  {now}\n");
            }

            print_health_dashboard(&targets, &ssh_data, &rpc_data);
        }

        if !follow {
            return Ok(());
        }

        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(interval)) => {}
            _ = tokio::signal::ctrl_c() => return Ok(())

        }
    }
}

// ── SSH collection ───────────────────────────────────────────────────────────

/// Build (name, private_ip) targets from infra data.
/// Uses the first subnet IP (insertion order = primary ENI in remote mode) as the private IP for SSH.
///
/// - `"all"` → CC + all nodes
/// - `"cc"`  → CC only
/// - other   → the single matching node
fn build_targets(infra_data: &InfraData, node_or_cc: &str) -> SshTargets {
    match node_or_cc {
        "all" => {
            let mut targets = Vec::new();
            if let Some(cc) = infra_data.control_center.as_ref() {
                targets.push((CC_TITLE.to_string(), cc.first_private_ip().to_owned()));
            }
            for (name, node) in infra_data.nodes.iter() {
                targets.push((name.clone(), node.first_private_ip().to_owned()));
            }
            targets
        }
        "cc" => infra_data
            .control_center
            .as_ref()
            .map(|cc| vec![(CC_TITLE.to_string(), cc.first_private_ip().to_owned())])
            .unwrap_or_default(),
        node => infra_data
            .nodes
            .get(node)
            .map(|n| vec![(node.to_string(), n.first_private_ip().to_owned())])
            .unwrap_or_default(),
    }
}

fn is_cc(name: &str) -> bool {
    name == CC_TITLE
}

/// Reads /proc directly (no awk/$-signs) so it survives multiple levels of
/// SSH double-quote wrapping. `echo DF; df -h /` emits a marker line
/// followed by the df header and data line, parsed in `parse_resource_output`.
const COLLECT_CMD: &str = "head -3 /proc/meminfo; head -1 /proc/loadavg; nproc; \
    echo DF; df -h /; \
    docker stats --no-stream --format {{.Name}}::{{.MemUsage}} 2>/dev/null";

/// Collect resource usage + container state from the given targets via SSH.
///
/// When targets contain CC as the first entry, fans out to all nodes in
/// parallel via [`RemoteInfra::ssh_fanout_with_output`]. When targets contain
/// a single node, SSHs to that node directly via CC hop.
fn collect_ssh_usage(targets: &SshTargets, remote: &RemoteInfra) -> Result<HostUsageMap> {
    if targets.is_empty() {
        return Ok(HashMap::default());
    }

    if !is_cc(&targets[0].0) {
        let (_, ip) = &targets[0];
        let script = format!(
            "ssh {} {}@{ip} '{COLLECT_CMD}'",
            remote::CC_SSH_OPTS,
            remote::USER_NAME,
        );
        let output = remote.ssh_cc_with_output(&script)?;
        let usage = parse_resource_output(&output);
        return Ok(HashMap::from([(ip.clone(), usage)]));
    }

    let cc_ip = &targets[0].1;
    let node_ips: Vec<&str> = targets.iter().skip(1).map(|(_, ip)| ip.as_str()).collect();

    let output = remote.ssh_fanout_with_output(cc_ip, &node_ips, COLLECT_CMD)?;
    Ok(parse_multi_host_output(&output))
}

// ── RPC collection ───────────────────────────────────────────────────────────

async fn collect_rpc_data(node_urls: &[(NodeName, Url)]) -> HashMap<NodeName, RpcSnapshot> {
    let futures = node_urls.iter().map(|(name, url)| {
        let name = name.clone();
        let client = rpc::RpcClient::new(url.clone(), Duration::from_secs(2));
        let c2 = client.clone();
        async move {
            let (height, peers) = tokio::join!(
                client.get_latest_block_number_with_retries(0),
                c2.get_peers(),
            );
            let snap = RpcSnapshot {
                height,
                peer_count: peers.map(|p| p.len()),
            };
            (name, snap)
        }
    });
    futures::future::join_all(futures)
        .await
        .into_iter()
        .collect()
}

// ── Dashboard rendering ──────────────────────────────────────────────────────

const RED: &str = "\x1b[31m";
const RST: &str = "\x1b[0m";

/// Print the column header for single-host time-series output.
///
/// `container_names` are discovered from the first SSH tick and used as
/// dynamic column headers.
fn print_single_host_header(is_cc: bool, container_names: &[String]) {
    let mut hdr = "Time                |".to_string();
    if !is_cc {
        hdr.push_str(" Height | Peers |");
    }
    hdr.push_str("    Mem  Total | CPU% |   Disk Total |");
    for c in container_names {
        let width = c.len().max(14);
        hdr.push_str(&format!(" {:<width$} |", c));
    }
    println!("{hdr}");
}

/// Print one time-series data row for a single host.
fn print_single_host_row(
    target: &(NodeName, IpAddress),
    ssh_data: &HashMap<IpAddress, Result<HostUsage>>,
    rpc_data: &HashMap<NodeName, RpcSnapshot>,
    is_cc: bool,
    container_names: &[String],
) {
    let (name, ip) = target;
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S");

    let mut row = format!("{now} |");

    if !is_cc {
        if let Some(snap) = rpc_data.get(name) {
            let h = match &snap.height {
                Ok(h) => format!("{h:>6}"),
                Err(_) => format!("{RED}   err{RST}"),
            };
            let peers = match &snap.peer_count {
                Ok(0) => format!("{RED}    0{RST}"),
                Ok(n) => format!("{n:>5}"),
                Err(_) => format!("{RED}  err{RST}"),
            };
            row.push_str(&format!(" {h} | {peers} |"));
        } else {
            row.push_str("      - |     - |");
        }
    }

    if let Some(usage_result) = ssh_data.get(ip) {
        match usage_result {
            Ok(u) => {
                let cpu = if u.cpu_pct > 80.0 {
                    format!("{RED}{:>4}{RST}", u.cpu_percent)
                } else {
                    format!("{:>4}", u.cpu_percent)
                };
                row.push_str(&format!(
                    " {:>6} {:>6} | {cpu} | {:>6} {:>5} |",
                    u.mem_used, u.mem_total, u.disk_used, u.disk_total,
                ));
                for cname in container_names {
                    let mem = u
                        .containers
                        .iter()
                        .find(|c| c.name == *cname)
                        .map(|c| format_container_mem(&c.mem_usage, &c.mem_limit))
                        .unwrap_or_else(|| "-".to_string());
                    let width = cname.len().max(mem.len()).max(14);
                    row.push_str(&format!(" {mem:>width$} |"));
                }
            }
            Err(e) => {
                row.push_str(&format!(" {RED}{}{RST}", truncate_error(e)));
            }
        }
    } else {
        row.push_str(&format!(" {RED}no response{RST}"));
    }

    println!("{row}");
}

fn print_health_dashboard(
    targets: &[(NodeName, IpAddress)],
    ssh_data: &HashMap<IpAddress, Result<HostUsage>>,
    rpc_data: &HashMap<NodeName, RpcSnapshot>,
) {
    let max_name_len = targets.iter().map(|(n, _)| n.len()).max().unwrap_or(14);

    // Find the longest container name and formatted memory string length
    let (max_cname, max_cmem) = targets
        .iter()
        .filter_map(|(_, ip)| ssh_data.get(ip)?.as_ref().ok())
        .flat_map(|u| &u.containers)
        .fold((0usize, 0usize), |(cn, cm), c| {
            let mem_len = format_container_mem(&c.mem_usage, &c.mem_limit).len();
            (cn.max(c.name.len()), cm.max(mem_len))
        });

    println!(
        "{:<max_name_len$} | Height | Peers |    Mem  Total | CPU% |   Disk Total | Containers",
        ""
    );

    let mut heights: Vec<u64> = rpc_data
        .values()
        .filter_map(|s| s.height.as_ref().ok().copied())
        .collect();
    heights.sort_unstable();
    let median_height = if heights.is_empty() {
        0u64
    } else {
        heights[heights.len() / 2]
    };

    for (name, ip) in targets {
        let mut row = format!("{:<max_name_len$} |", name);

        // RPC data (height and peer count)
        if let Some(snap) = rpc_data.get(name) {
            let h = match &snap.height {
                Ok(h) => {
                    if median_height.saturating_sub(*h) > 5 {
                        format!("{RED}{h:>6}{RST}")
                    } else {
                        format!("{h:>6}")
                    }
                }
                Err(_) => format!("{RED}  err{RST}"),
            };
            let peers = match &snap.peer_count {
                Ok(0) => format!("{RED}    0{RST}"),
                Ok(n) => format!("{n:>5}"),
                Err(_) => format!("{RED}  err{RST}"),
            };
            row.push_str(&format!(" {h} | {peers} |"));
        } else {
            row.push_str("      - |     - |");
        }

        // SSH data (memory usage, CPU usage, container usage)
        if let Some(usage_result) = ssh_data.get(ip) {
            match usage_result {
                Ok(u) => {
                    let cpu = if u.cpu_pct > 80.0 {
                        format!("{RED}{:>4}{RST}", u.cpu_percent)
                    } else {
                        format!("{:>4}", u.cpu_percent)
                    };
                    row.push_str(&format!(
                        " {:>6} {:>6} | {cpu} | {:>6} {:>5} |",
                        u.mem_used, u.mem_total, u.disk_used, u.disk_total,
                    ));

                    let parts: Vec<String> = u
                        .containers
                        .iter()
                        .map(|c| {
                            let max_cname = if is_cc(name) { max_cname } else { 0 };
                            let mem = format_container_mem(&c.mem_usage, &c.mem_limit);
                            format!("{:<max_cname$}: {mem:>max_cmem$}", c.name)
                        })
                        .collect();

                    if !is_cc(name) {
                        row.push_str(&format!(" {}", parts.join(", ")));
                    } else if let Some((first, rest)) = parts.split_first() {
                        let pad = display_width(&row);
                        row.push_str(&format!(" {first}"));
                        for p in rest {
                            println!("{row}");
                            row = format!("{:pad$} {p}", "");
                        }
                    }
                }
                Err(e) => {
                    row.push_str(&format!(" {RED}{}{RST}", truncate_error(e)));
                }
            }
        } else {
            row.push_str(&format!(" {RED}no response{RST}"));
        }

        println!("{row}");
    }
}

/// Parse a docker memory string (e.g. "294.4MiB", "1.864GiB", "15.2kB") into bytes.
fn parse_docker_mem(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Some(n) = s.strip_suffix("GiB") {
        n.parse::<f64>().ok().map(|v| v * 1024.0 * 1024.0 * 1024.0)
    } else if let Some(n) = s.strip_suffix("MiB") {
        n.parse::<f64>().ok().map(|v| v * 1024.0 * 1024.0)
    } else if let Some(n) = s.strip_suffix("KiB").or_else(|| s.strip_suffix("kiB")) {
        n.parse::<f64>().ok().map(|v| v * 1024.0)
    } else if let Some(n) = s.strip_suffix("kB").or_else(|| s.strip_suffix("KB")) {
        n.parse::<f64>().ok().map(|v| v * 1000.0)
    } else if let Some(n) = s.strip_suffix('B') {
        n.parse::<f64>().ok()
    } else {
        None
    }
}

/// Format container memory as "used / limit" in MiB.
///
/// E.g. "294.4MiB" + "1.864GiB" → "294 / 1909 MiB"
fn format_container_mem(usage: &str, limit: &str) -> String {
    let (Some(u), Some(l)) = (parse_docker_mem(usage), parse_docker_mem(limit)) else {
        return format!("{usage} / {limit}");
    };
    let mib = 1024.0 * 1024.0;
    format!("{:.0} / {:.0} MiB", u / mib, l / mib)
}

// ── Parsing helpers ──────────────────────────────────────────────────────────

/// Reformat a `df -h` value (e.g. "15G", "2.3G", "456M") with 2 decimal places.
fn format_df_value(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return "-".to_string();
    }
    let (num_part, suffix) = s.split_at(s.len().saturating_sub(1));
    if let Ok(v) = num_part.parse::<f64>() {
        format!("{v:.2}{suffix}")
    } else {
        s.to_string()
    }
}

fn format_kb(kb: u64) -> String {
    if kb >= 1024 * 1024 {
        format!("{:.1}Gi", kb as f64 / (1024.0 * 1024.0))
    } else if kb >= 1024 {
        format!("{:.0}Mi", kb as f64 / 1024.0)
    } else {
        format!("{kb}kB")
    }
}

/// Parse kB value from a /proc/meminfo line like "MemTotal:  16099284 kB".
fn parse_meminfo_kb(line: &str) -> Option<u64> {
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Parse combined output from CC that contains multiple HOST: delimited blocks.
///
/// Each block contains raw /proc/meminfo, /proc/loadavg, nproc, and docker
/// stats output that gets parsed into structured data.
fn parse_multi_host_output(output: &str) -> HashMap<IpAddress, Result<HostUsage>> {
    let mut results = HashMap::default();
    let mut current_ip: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();

    for line in output.lines() {
        if let Some(ip) = line.strip_prefix("HOST:") {
            if let Some(prev_ip) = current_ip.take() {
                let block = current_lines.join("\n");
                results.insert(prev_ip, parse_resource_output(&block));
                current_lines.clear();
            }
            current_ip = Some(ip.trim().to_string());
        } else if current_ip.is_some() {
            current_lines.push(line.to_string());
        }
    }
    if let Some(ip) = current_ip {
        let block = current_lines.join("\n");
        results.insert(ip, parse_resource_output(&block));
    }

    results
}

/// Parse raw output from a single host into structured usage data.
///
/// Expected input (lines):
///   MemTotal:       16099284 kB
///   MemFree:        13456784 kB
///   MemAvailable:   14567892 kB
///   0.25 0.15 0.10 1/150 12345
///   2
///   cl::37.98MiB / 1.864GiB
///   el::921.5MiB / 1.864GiB
fn parse_resource_output(output: &str) -> Result<HostUsage> {
    let mut mem_total_kb: u64 = 0;
    let mut mem_available_kb: u64 = 0;
    let mut load_avg: f64 = 0.0;
    let mut nproc: u64 = 1;
    let mut disk_total = String::new();
    let mut disk_used = String::new();
    let mut in_df = false;
    let mut expect_nproc = false;
    let mut containers = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "DF" {
            in_df = true;
            continue;
        }
        if in_df {
            if line.starts_with("Filesystem") {
                continue;
            }
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() >= 5 {
                disk_total = cols[1].to_string();
                disk_used = format_df_value(cols[2]);
            }
            in_df = false;
            continue;
        }
        if expect_nproc {
            expect_nproc = false;
            if let Ok(v) = line.parse::<u64>() {
                nproc = v;
            }
            continue;
        }
        if line.starts_with("MemTotal:") {
            mem_total_kb = parse_meminfo_kb(line).unwrap_or(0);
        } else if line.starts_with("MemAvailable:") {
            mem_available_kb = parse_meminfo_kb(line).unwrap_or(0);
        } else if line.starts_with("MemFree:") || line.starts_with("CS:") {
            // skip — we use MemAvailable instead of MemFree; CS: container state no longer needed
        } else if line.contains("::") {
            if let Some((name, mem)) = line.split_once("::") {
                if let Some((usage, limit)) = mem.split_once('/') {
                    containers.push(ContainerUsage {
                        name: name.trim().to_string(),
                        mem_usage: usage.trim().to_string(),
                        mem_limit: limit.trim().to_string(),
                    });
                }
            }
        } else if let Some(first) = line.split_whitespace().next() {
            if let Ok(v) = first.parse::<f64>() {
                if line.contains('/') && line.split_whitespace().count() >= 4 {
                    load_avg = v;
                    expect_nproc = true;
                }
            }
        }
    }

    if mem_total_kb == 0 {
        return Err(eyre::eyre!("no meminfo"));
    }

    let mem_used_kb = mem_total_kb.saturating_sub(mem_available_kb);
    let cpu_pct = load_avg / nproc as f64 * 100.0;

    Ok(HostUsage {
        mem_total: format_kb(mem_total_kb),
        mem_used: format_kb(mem_used_kb),
        cpu_percent: format!("{cpu_pct:.0}%"),
        cpu_pct,
        disk_total,
        disk_used,
        containers,
    })
}

/// Visible (display) width of a string, ignoring ANSI escape sequences.
fn display_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut in_escape = false;
    for b in s.bytes() {
        if in_escape {
            if b.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else if b == b'\x1b' {
            in_escape = true;
        } else {
            width += 1;
        }
    }
    width
}

fn truncate_error(e: &color_eyre::eyre::Error) -> String {
    let msg = e.to_string();
    if msg.len() > 50 {
        format!("{}...", &msg[..msg.floor_char_boundary(47)])
    } else {
        msg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_docker_mem_gib() {
        let bytes = parse_docker_mem("1.5GiB").unwrap();
        assert!((bytes - 1.5 * 1024.0 * 1024.0 * 1024.0).abs() < 1.0);
    }

    #[test]
    fn parse_docker_mem_mib() {
        let bytes = parse_docker_mem("294.4MiB").unwrap();
        assert!((bytes - 294.4 * 1024.0 * 1024.0).abs() < 1.0);
    }

    #[test]
    fn parse_docker_mem_kib() {
        let bytes = parse_docker_mem("512KiB").unwrap();
        assert!((bytes - 512.0 * 1024.0).abs() < 1.0);

        let bytes = parse_docker_mem("512kiB").unwrap();
        assert!((bytes - 512.0 * 1024.0).abs() < 1.0);
    }

    #[test]
    fn parse_docker_mem_kb_decimal() {
        let bytes = parse_docker_mem("15.2kB").unwrap();
        assert!((bytes - 15.2 * 1000.0).abs() < 1.0);

        let bytes = parse_docker_mem("15.2KB").unwrap();
        assert!((bytes - 15.2 * 1000.0).abs() < 1.0);
    }

    #[test]
    fn parse_docker_mem_bare_bytes() {
        let bytes = parse_docker_mem("1024B").unwrap();
        assert!((bytes - 1024.0).abs() < 1.0);
    }

    #[test]
    fn parse_docker_mem_whitespace() {
        let bytes = parse_docker_mem("  294.4MiB  ").unwrap();
        assert!((bytes - 294.4 * 1024.0 * 1024.0).abs() < 1.0);
    }

    #[test]
    fn parse_docker_mem_invalid() {
        assert!(parse_docker_mem("").is_none());
        assert!(parse_docker_mem("abc").is_none());
        assert!(parse_docker_mem("notanumberMiB").is_none());
    }
}
