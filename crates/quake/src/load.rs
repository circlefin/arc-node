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

use std::path::Path;

use color_eyre::eyre::{self, bail, Result};
use tracing::warn;

use crate::genesis;
use crate::infra::InfraType;
use crate::manifest::Manifest;
use crate::node::NodeName;
use crate::testnet::{Testnet, QUAKE_DIR};
use spammer::{self, Spammer, SpammerArgs};

/// Dispatch a `quake load` or `quake spam` invocation against `testnet`.
///
/// `fire_and_forget` controls whether the generated transactions wait for
/// receipts (`false` for `load`, `true` for `spam`). `silent` propagates the
/// top-level verbosity flag to the local spammer's config.
pub(crate) async fn run(
    testnet: &Testnet,
    target_nodes: Vec<NodeName>,
    args: &SpammerArgs,
    fire_and_forget: bool,
    silent: bool,
) -> Result<()> {
    match testnet.infra_data.infra_type {
        InfraType::Local => {
            let config = args.to_config(silent, fire_and_forget);
            config.validate()?;
            load(testnet, target_nodes, &config).await
        }
        InfraType::Remote => load_remote(testnet, target_nodes, args, fire_and_forget),
    }
}

/// Generate and send transaction load to a local testnet.
pub(crate) async fn load(
    testnet: &Testnet,
    target_nodes: Vec<NodeName>,
    config: &spammer::Config,
) -> Result<()> {
    let target_nodes = resolve_load_target_nodes(&testnet.manifest, &target_nodes)?;

    // Build EL WebSocket URLs of target nodes
    let target_ws_urls = testnet.nodes_metadata.to_execution_ws_urls(&target_nodes);

    // Calculate from genesis the number of extra prefunded accounts and update the config
    let num_extra_accounts = genesis::num_prefunded_accounts(
        &testnet.dir.join("assets").join("genesis.json"),
        testnet.manifest.num_validators(),
    )?;

    // Store latency CSV under .quake/results/<testnet-name>/
    let testnet_name = testnet
        .dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre::eyre!("cannot derive testnet name from dir"))?;
    let csv_dir = Path::new(QUAKE_DIR).join("results").join(testnet_name);

    let config = spammer::Config {
        max_num_accounts: std::cmp::min(num_extra_accounts, config.max_num_accounts),
        csv_dir: Some(csv_dir),
        ..*config
    };

    let spammer = Spammer::new(target_ws_urls, &config).await?;
    spammer.run().await
}

/// Send transaction load to a remote testnet via SSH to the Control Center.
///
/// Converts typed `SpammerArgs` + targets into the CLI format expected by
/// the remote `spammer.sh` script, expands node group selectors locally,
/// and executes over SSH.
pub(crate) fn load_remote(
    testnet: &Testnet,
    target_nodes: Vec<NodeName>,
    args: &SpammerArgs,
    fire_and_forget: bool,
) -> Result<()> {
    let infra = testnet.remote_infra()?;
    if args.csv_dir.is_some() {
        warn!(
            "--csv-dir is ignored on remote testnets; \
             latency CSV would land on the inaccessible Control Center."
        );
    }
    let mut cli_args = args.to_cli_args();
    if !target_nodes.is_empty() {
        cli_args.push("--targets".to_string());
        cli_args.push(target_nodes.join(","));
    }
    let cmd = build_remote_spammer_cmd(&testnet.manifest, &cli_args, fire_and_forget)?;
    infra.ssh_cc(&cmd.join(" "), false)
}

/// Resolve local `quake load` and `quake spam` targets to concrete node names.
///
/// This helper keeps load/spam selector semantics aligned with the manifest:
/// an empty selector list means "all nodes", while a non-empty list may
/// contain exact node names or manifest node-group names.
///
/// Explicit selectors must resolve to at least one node. Load generation
/// against an empty target set is treated as an error.
fn resolve_load_target_nodes(manifest: &Manifest, selectors: &[NodeName]) -> Result<Vec<NodeName>> {
    if selectors.is_empty() {
        return Ok(manifest.nodes.keys().cloned().collect());
    }

    let target_nodes = manifest.resolve_node_selectors(selectors)?;
    if target_nodes.is_empty() {
        bail!("load/spam targets resolved to no nodes");
    }

    Ok(target_nodes)
}

/// Split remote `quake load/spam...` args into spammer flags and targets.
///
/// Quake only modifies the `--targets` segment. All other args are passed through
/// to the remote `spammer` process unchanged.
///
/// Examples:
/// - `["--rate", "42", "--targets", "validator1,RPC_NODES"]` becomes:
///   - forwarded args: `["--rate", "42"]`
///   - target selectors: `["validator1", "RPC_NODES"]`
/// - `["--targets=validator1,RPC_NODES", "--time", "5"]` becomes:
///   - forwarded args: `["--time", "5"]`
///   - target selectors: `["validator1", "RPC_NODES"]`
/// - `["--targets", "validator1", "--time", "5"]` becomes:
///   - forwarded args: `["--time", "5"]`
///   - target selectors: `["validator1"]`
///
/// The returned selectors are later expanded against the manifest, and the final
/// remote spammer command gets a normalized
/// `--targets <expanded-node>...` segment appended at the end.
fn split_remote_targets(args: &[String]) -> Result<(Vec<String>, Vec<NodeName>)> {
    let mut forwarded_args = Vec::new();
    let mut target_selectors = Vec::new();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];

        if arg != "--targets" && !arg.starts_with("--targets=") {
            forwarded_args.push(arg.clone());
            index += 1;
            continue;
        }

        let targets = if let Some(args_targets) = arg.strip_prefix("--targets=") {
            if args_targets.is_empty() {
                bail!("remote load/spam `--targets` requires a comma-separated target list");
            }
            index += 1;
            args_targets.to_string()
        } else {
            index += 1;
            // covers both the case where `--targets` is the last arg, and the case
            // where it's followed by another flag (e.g. `--time`) without a value
            // (e.g. `--targets --time 5`)
            if index >= args.len() || args[index].starts_with('-') {
                bail!("remote load/spam `--targets` requires a comma-separated target list");
            }
            let args_targets = args[index].clone();
            index += 1;
            // covers the case where `--targets` is followed by more than one value
            // (e.g. `--targets val1,val2 val3`), which is incorrect.
            // Notice the space between `val2` and `val3`, but `val3` is not a flag,
            // and should be attached to the `--targets` value with a comma instead.
            // An example of valid syntax is `--targets val1,val2 --time 5`.
            if index < args.len() && !args[index].starts_with('-') {
                bail!("remote load/spam `--targets` must use one comma-separated target list");
            }
            args_targets
        };

        for target in targets.split(',') {
            if target.is_empty() {
                bail!("remote load/spam `--targets` requires non-empty target values");
            }
            target_selectors.push(target.to_string());
        }
    }

    Ok((forwarded_args, target_selectors))
}

/// Build the `spammer nodes` command for remote `quake load/spam`.
///
/// Strips only the `--targets` segment to expand manifest node groups
/// locally, and appends explicit node names as one comma-delimited
/// `--targets` value.
pub(crate) fn build_remote_spammer_cmd(
    manifest: &Manifest,
    args: &[String],
    fire_and_forget: bool,
) -> Result<Vec<String>> {
    let (forwarded_args, target_selectors) = split_remote_targets(args)?;
    let mut cmd = vec![
        "./spammer.sh".to_string(),
        "nodes".to_string(),
        "--nodes-path".to_string(),
        "nodes.json".to_string(),
    ];

    if fire_and_forget {
        cmd.push("--fire-and-forget".to_string());
    }

    cmd.extend(forwarded_args);

    if !target_selectors.is_empty() {
        let target_nodes = resolve_load_target_nodes(manifest, &target_selectors)?;
        cmd.push("--targets".to_string());
        cmd.push(target_nodes.join(","));
    }

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Node, NodeType};
    use indexmap::IndexMap;

    fn remote_manifest() -> Manifest {
        let mut nodes = IndexMap::new();
        nodes.insert("validator1".to_string(), Node::default());
        nodes.insert("validator2".to_string(), Node::default());
        nodes.insert(
            "full1".to_string(),
            Node {
                node_type: NodeType::NonValidator,
                ..Node::default()
            },
        );

        let mut node_groups = IndexMap::new();
        node_groups.insert(
            "TRUSTED".to_string(),
            vec!["ALL_VALIDATORS".to_string(), "full1".to_string()],
        );

        Manifest {
            nodes,
            node_groups,
            ..Manifest::default()
        }
    }

    fn validators_only_manifest() -> Manifest {
        let mut nodes = IndexMap::new();
        nodes.insert("validator1".to_string(), Node::default());
        nodes.insert("validator2".to_string(), Node::default());

        Manifest {
            nodes,
            ..Manifest::default()
        }
    }

    #[test]
    fn split_remote_targets_success_cases() {
        struct Case {
            name: &'static str,
            args: Vec<&'static str>,
            expected_forwarded: Vec<&'static str>,
            expected_targets: Vec<&'static str>,
        }

        let cases = vec![
            Case {
                name: "no targets flag",
                args: vec!["--rate", "42", "--time", "5"],
                expected_forwarded: vec!["--rate", "42", "--time", "5"],
                expected_targets: vec![],
            },
            Case {
                name: "targets in middle of argv",
                args: vec![
                    "--rate",
                    "42",
                    "--targets",
                    "validator1,ALL_VALIDATORS",
                    "--mix",
                    "transfer=70,erc20=30",
                ],
                expected_forwarded: vec!["--rate", "42", "--mix", "transfer=70,erc20=30"],
                expected_targets: vec!["validator1", "ALL_VALIDATORS"],
            },
            Case {
                name: "targets at beginning of argv",
                args: vec!["--targets", "validator1,TRUSTED", "--rate", "42"],
                expected_forwarded: vec!["--rate", "42"],
                expected_targets: vec!["validator1", "TRUSTED"],
            },
            Case {
                name: "inline targets in middle of argv",
                args: vec![
                    "--rate",
                    "42",
                    "--targets=validator1,ALL_VALIDATORS",
                    "--time",
                    "5",
                ],
                expected_forwarded: vec!["--rate", "42", "--time", "5"],
                expected_targets: vec!["validator1", "ALL_VALIDATORS"],
            },
        ];

        for case in cases {
            let args: Vec<String> = case.args.iter().map(|s| s.to_string()).collect();
            let (forwarded, targets) =
                split_remote_targets(&args).expect("split_remote_targets should succeed");
            assert_eq!(
                forwarded, case.expected_forwarded,
                "case '{}': forwarded args mismatch",
                case.name,
            );
            assert_eq!(
                targets, case.expected_targets,
                "case '{}': target selectors mismatch",
                case.name,
            );
        }
    }

    #[test]
    fn split_remote_targets_err_cases() {
        struct Case {
            name: &'static str,
            args: Vec<&'static str>,
            expected_message: &'static str,
        }

        let cases = vec![
            Case {
                name: "standalone targets flag last arg without value",
                args: vec!["--rate", "42", "--targets"],
                expected_message: "`--targets` requires a comma-separated target list",
            },
            Case {
                name: "inline targets flag without value",
                args: vec!["--rate", "42", "--targets="],
                expected_message: "`--targets` requires a comma-separated target list",
            },
            Case {
                name: "space-separated targets are rejected",
                args: vec!["--rate", "42", "--targets", "val1,val2", "val3"],
                expected_message: "`--targets` must use one comma-separated target list",
            },
        ];

        for case in cases {
            let args: Vec<String> = case.args.iter().map(|s| s.to_string()).collect();
            let err = split_remote_targets(&args).unwrap_err();
            assert!(
                err.to_string().contains(case.expected_message),
                "case '{}': unexpected error: {err}",
                case.name,
            );
        }
    }

    #[test]
    fn build_remote_spammer_cmd_expands_group_targets() {
        struct Case<'a> {
            name: &'a str,
            args: &'a [&'a str],
            fire_and_forget: bool,
            expected_cmd: &'a [&'a str],
        }

        let manifest = remote_manifest();
        let cases = vec![
            Case {
                name: "no targets flag",
                args: &["--rate", "42", "--time", "5"],
                fire_and_forget: false,
                expected_cmd: &[
                    "./spammer.sh",
                    "nodes",
                    "--nodes-path",
                    "nodes.json",
                    "--rate",
                    "42",
                    "--time",
                    "5",
                ],
            },
            Case {
                name: "standalone targets flag",
                args: &["--rate", "42", "--targets", "TRUSTED", "--time", "5"],
                fire_and_forget: true,
                expected_cmd: &[
                    "./spammer.sh",
                    "nodes",
                    "--nodes-path",
                    "nodes.json",
                    "--fire-and-forget",
                    "--rate",
                    "42",
                    "--time",
                    "5",
                    "--targets",
                    "validator1,validator2,full1",
                ],
            },
            Case {
                name: "inline targets flag",
                args: &["--rate", "42", "--targets=TRUSTED", "--time", "5"],
                fire_and_forget: false,
                expected_cmd: &[
                    "./spammer.sh",
                    "nodes",
                    "--nodes-path",
                    "nodes.json",
                    "--rate",
                    "42",
                    "--time",
                    "5",
                    "--targets",
                    "validator1,validator2,full1",
                ],
            },
        ];

        for case in cases {
            let args: Vec<String> = case.args.iter().map(|s| s.to_string()).collect();
            let cmd = build_remote_spammer_cmd(&manifest, &args, case.fire_and_forget)
                .expect("build_remote_spammer_cmd should succeed");
            assert_eq!(
                cmd, case.expected_cmd,
                "case '{}': remote spammer command mismatch",
                case.name,
            );
        }
    }

    #[test]
    fn resolve_load_target_nodes_rejects_empty_expansion() {
        let manifest = validators_only_manifest();
        let selectors = vec!["ALL_NON_VALIDATORS".to_string()];

        let err = resolve_load_target_nodes(&manifest, &selectors).unwrap_err();
        assert!(
            err.to_string()
                .contains("load/spam targets resolved to no nodes"),
            "unexpected error: {err}",
        );
    }

    #[test]
    fn build_remote_spammer_cmd_rejects_empty_expansion() {
        let manifest = validators_only_manifest();
        let args = vec!["--targets".to_string(), "ALL_NON_VALIDATORS".to_string()];

        let err = build_remote_spammer_cmd(&manifest, &args, false).unwrap_err();
        assert!(
            err.to_string()
                .contains("load/spam targets resolved to no nodes"),
            "unexpected error: {err}",
        );
    }
}
